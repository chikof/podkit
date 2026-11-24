//! Runs the checks that are due. Called on every turn of the jobs dispatcher's loop, like the
//! notification delivery pass: there is no service of its own, one pass at a time.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use time::OffsetDateTime;

use super::probe::{self, HttpSpec, Outcome};
use super::{CHECK_COLUMNS, Check, Kind, State, Step, advance};
use crate::ids::UptimeIncidentId;
use crate::net::outbound::Outbound;
use crate::notifications::event::{Event, EventKind, clip};
use crate::notifications::hooks;
use crate::state::AppState;

/// Checks claimed by one pass.
const BATCH: i64 = 64;
/// Checks running at the same time.
const CONCURRENCY: usize = 16;
/// While a check that is not down yet keeps failing it is asked again this often, so that the
/// failures needed to call it down do not take three whole intervals.
const RETRY_SECS: i32 = 30;
/// The certificate is looked at again after this long.
const TLS_RECHECK: time::Duration = time::Duration::hours(6);
/// Application domains are looked for this often.
const RECONCILE_EVERY: i64 = 60;

static RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_RECONCILE: AtomicI64 = AtomicI64::new(0);

/// Runs one pass in the background unless one is already running.
pub fn wake(state: &AppState) {
	if RUNNING.swap(true, Ordering::AcqRel) {
		return;
	}
	let state = state.clone();
	state.jobs.clone().tracker.spawn(async move {
		let now = OffsetDateTime::now_utc();
		if now.unix_timestamp() - LAST_RECONCILE.load(Ordering::Relaxed) >= RECONCILE_EVERY {
			LAST_RECONCILE.store(now.unix_timestamp(), Ordering::Relaxed);
			if let Err(e) = super::service::reconcile_auto(&state).await {
				tracing::warn!("cannot make the uptime checks for application domains: {e}");
			}
		}
		if let Err(e) = tick(&state, now).await {
			tracing::warn!("uptime pass failed: {e}");
		}
		RUNNING.store(false, Ordering::Release);
	});
}

/// Claims every check that is due (at most [`BATCH`]), pushes its next run one interval out and
/// runs them, [`CONCURRENCY`] at a time. Returns how many ran.
///
/// # Errors
/// The database rejects the claim.
pub async fn tick(state: &AppState, now: OffsetDateTime) -> Result<usize, sqlx::Error> {
	let due = sqlx::query_as::<_, Check>(sqlx::AssertSqlSafe(format!(
		"UPDATE uptime_checks SET next_check_at = $1 + make_interval(secs => interval_secs)
		WHERE id IN (SELECT id FROM uptime_checks WHERE enabled AND next_check_at <= $1
			ORDER BY next_check_at LIMIT $2 FOR UPDATE SKIP LOCKED)
		RETURNING {CHECK_COLUMNS}"
	)))
	.bind(now)
	.bind(BATCH)
	.fetch_all(&state.db)
	.await?;
	let ran = due.len();
	futures_util::stream::iter(due)
		.for_each_concurrent(CONCURRENCY, |check| async move {
			run_one(state, check, now).await;
		})
		.await;
	Ok(ran)
}

/// What a check finds when it runs: the probe, and a look at the certificate when one is due.
pub async fn probe(
	outbound: &Outbound,
	check: &Check,
	now: OffsetDateTime,
) -> (Outcome, Option<Result<OffsetDateTime, String>>) {
	let timeout = Duration::from_secs(u64::try_from(check.timeout_secs).unwrap_or(10));
	match check.kind() {
		Kind::Tcp => {
			let host = check.host.as_deref().unwrap_or_default();
			let port = u16::try_from(check.port.unwrap_or(0)).unwrap_or(0);
			(probe::tcp(outbound, host, port, timeout).await, None)
		}
		Kind::Http => {
			let url = check.url.clone().unwrap_or_default();
			let spec = HttpSpec {
				url: url.clone(),
				head: check.method == "HEAD",
				expected_status: u16::try_from(check.expected_status).unwrap_or(0),
				keyword: check.keyword.clone(),
				timeout,
			};
			let outcome = probe::http(outbound, &spec).await;
			let tls = match (check.tls_warn_days, url::Url::parse(&url)) {
				(Some(_), Ok(parsed))
					if parsed.scheme() == "https"
						&& check
							.tls_checked_at
							.is_none_or(|at| now - at >= TLS_RECHECK) =>
				{
					let host = parsed.host_str().unwrap_or_default().to_owned();
					let port = parsed.port_or_known_default().unwrap_or(443);
					Some(probe::tls_expiry(outbound, &host, port, timeout).await)
				}
				_ => None,
			};
			(outcome, tls)
		}
	}
}

async fn run_one(state: &AppState, check: Check, now: OffsetDateTime) {
	let (outcome, tls) = probe(&state.outbound, &check, now).await;
	if let Err(e) = record(state, &check, &outcome, tls, OffsetDateTime::now_utc()).await {
		tracing::warn!(check = %check.id, "cannot store an uptime result: {e}");
	}
}

/// Stores one run: the result row, the new state and, on a transition, the incident and the
/// notification. `tls` is what a look at the certificate found, when one was taken.
///
/// # Errors
/// The database rejects a write.
pub async fn record(
	state: &AppState,
	check: &Check,
	outcome: &Outcome,
	tls: Option<Result<OffsetDateTime, String>>,
	now: OffsetDateTime,
) -> Result<Step, sqlx::Error> {
	let db = &state.db;
	sqlx::query(
		"INSERT INTO uptime_results (check_id, at, ok, latency_ms, status_code, error)
		VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
	)
	.bind(check.id)
	.bind(now)
	.bind(outcome.ok)
	.bind(outcome.latency_ms)
	.bind(outcome.status_code)
	.bind(&outcome.error)
	.execute(db)
	.await?;

	let before = check.state();
	let (after, fails, oks, step) = advance(before, check.fail_streak, check.ok_streak, outcome.ok);
	let next = if !outcome.ok && after != State::Down {
		check.interval_secs.min(RETRY_SECS)
	} else {
		check.interval_secs
	};
	let (tls_expires, tls_at, mut warned) = match &tls {
		Some(Ok(at)) => (Some(*at), Some(now), check.tls_warned),
		Some(Err(_)) => (None, Some(now), check.tls_warned),
		None => (None, None, check.tls_warned),
	};
	let expiring = match (tls_expires.or(check.tls_expires_at), check.tls_warn_days) {
		(Some(at), Some(days)) => Some((at, super::probe::days_left(at, now) <= i64::from(days))),
		_ => None,
	};
	let warn_now = expiring.is_some_and(|(_, soon)| soon) && !warned && tls_expires.is_some();
	if let Some((_, soon)) = expiring
		&& tls_expires.is_some()
	{
		warned = soon;
	}
	sqlx::query(
		"UPDATE uptime_checks SET state = $2,
			state_since = CASE WHEN state <> $2 THEN $3 ELSE state_since END,
			fail_streak = $4, ok_streak = $5, last_checked_at = $3, last_latency_ms = $6,
			last_error = $7, next_check_at = $3 + make_interval(secs => $8),
			tls_expires_at = COALESCE($9, tls_expires_at), tls_checked_at = COALESCE($10, tls_checked_at),
			tls_warned = $11
		WHERE id = $1",
	)
	.bind(check.id)
	.bind(after.as_str())
	.bind(now)
	.bind(fails)
	.bind(oks)
	.bind(outcome.latency_ms)
	.bind(&outcome.error)
	.bind(f64::from(next))
	.bind(tls_expires)
	.bind(tls_at)
	.bind(warned)
	.execute(db)
	.await?;

	match step {
		Step::WentDown => {
			sqlx::query(
				"INSERT INTO uptime_incidents (id, check_id, started_at, error) VALUES ($1, $2, $3, $4)",
			)
			.bind(UptimeIncidentId::generate())
			.bind(check.id)
			.bind(now)
			.bind(&outcome.error)
			.execute(db)
			.await?;
			announce_down(state, check, outcome, fails).await;
		}
		Step::WentUp => {
			let started = sqlx::query_scalar::<_, OffsetDateTime>(
				"UPDATE uptime_incidents SET ended_at = $2 WHERE check_id = $1 AND ended_at IS NULL
				RETURNING started_at",
			)
			.bind(check.id)
			.bind(now)
			.fetch_optional(db)
			.await?;
			announce_up(state, check, started.map(|s| now - s)).await;
		}
		Step::Nothing => {}
	}
	if warn_now && let Some((at, _)) = expiring {
		announce_expiring(state, check, at, now).await;
	}
	Ok(step)
}

/// An event about a check, scoped to its application's project and environment when it has one.
async fn event(state: &AppState, check: &Check, kind: EventKind, title: String) -> Event {
	let link = format!("/teams/{}/uptime/{}", check.team_id, check.id);
	let subject = format!("check:{}", check.id);
	let target = match check.application_id {
		Some(app) => hooks::application(&state.db, app.0).await,
		None => None,
	};
	let event = Event::new(check.team_id, kind, subject, title).link(link);
	match target {
		Some(t) => event.scope(t.project, Some(t.environment)),
		None => event,
	}
}

async fn announce_down(state: &AppState, check: &Check, outcome: &Outcome, fails: i32) {
	let reason = outcome.error.as_deref().unwrap_or("no answer");
	let body = format!("{reason}. Failed {fails} runs in a row.");
	let title = format!("{} is down", clip(&check.name, 60));
	let ev = event(state, check, EventKind::UptimeDown, title)
		.await
		.body(body);
	crate::notifications::emit(state, ev).await;
}

async fn announce_up(state: &AppState, check: &Check, outage: Option<time::Duration>) {
	let body = outage.map_or_else(String::new, |d| format!("It was down for {}.", span(d)));
	let title = format!("{} is back up", clip(&check.name, 60));
	let ev = event(state, check, EventKind::UptimeUp, title)
		.await
		.body(body);
	crate::notifications::emit(state, ev).await;
}

async fn announce_expiring(
	state: &AppState,
	check: &Check,
	at: OffsetDateTime,
	now: OffsetDateTime,
) {
	let days = super::probe::days_left(at, now);
	let title = format!(
		"The certificate of {} expires in {} day{}",
		clip(&check.name, 60),
		days.max(0),
		if days == 1 { "" } else { "s" }
	);
	let mut ev = event(state, check, EventKind::CertExpiring, title).await;
	// A new expiry date is a new warning; the same one is not repeated by the dedup window.
	ev.subject = format!("check:{}:{}", check.id, at.unix_timestamp());
	crate::notifications::emit(state, ev).await;
}

/// `3 min`, `2 h 5 min`, `1 d 4 h`.
pub fn span(d: time::Duration) -> String {
	let secs = d.whole_seconds().max(0);
	let (days, hours, mins) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
	if days > 0 {
		format!("{days} d {hours} h")
	} else if hours > 0 {
		format!("{hours} h {mins} min")
	} else if mins > 0 {
		format!("{mins} min")
	} else {
		format!("{secs} s")
	}
}
