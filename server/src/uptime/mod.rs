//! Uptime checks: "is it up?" asked from outside, not just "does the container run".
//!
//! A check is an HTTP(S) request (method, expected status, optional keyword, timeout, optional
//! TLS expiry watch) or a TCP connect. Checks run from the control plane: [`runner::wake`] is
//! called on every turn of the jobs dispatcher's loop (there is no service of its own), claims
//! what is due, runs it with a concurrency limit and stores the result.
//!
//! * **History.** Raw results are kept for 7 days. [`rollup`] folds them into one row per check
//!   and UTC day, kept for 90 days, so the status page's uptime percentage and day strip reach
//!   back 90 days without 90 days of rows.
//! * **Incidents.** [`advance`] is the flap protection: a check goes down after
//!   [`DOWN_AFTER`] failed runs in a row (the runner retries every 30 s meanwhile) and comes
//!   back after [`UP_AFTER`] good ones. Only those transitions open and close an incident and
//!   emit `uptime_down` / `uptime_up` (see [`crate::notifications`]); a certificate that
//!   is about to expire emits `cert_expiring` once.
//! * **Status page.** A team may switch on a public `/status/<team slug>` that lists the checks
//!   it picked under the names it chose. It never shows a URL, host or port.
//! * **Auto checks.** Unless a team opted out, every application domain gets a check of its own
//!   ([`service::reconcile_auto`]). Deleting one remembers it, so it does not come back.

pub mod probe;
mod routes;
pub mod runner;
pub mod service;
#[cfg(test)]
mod tests;

use serde::Serialize;
use sqlx::PgPool;
use time::{Date, Duration, OffsetDateTime};

pub use routes::router;

use crate::ids::{ApplicationId, TeamId, UptimeCheckId};
use crate::metrics::Point;

/// Failed runs in a row before a check is down.
pub const DOWN_AFTER: i32 = 3;
/// Good runs in a row before a down check is up again.
pub const UP_AFTER: i32 = 2;
/// The shortest interval a check may have.
pub const MIN_INTERVAL_SECS: i32 = 30;
/// Raw results are dropped after this long.
pub const RAW_RETENTION: Duration = Duration::days(7);
/// Daily rows are dropped after this many days.
pub const DAILY_RETENTION_DAYS: i64 = 90;
/// Most points a response-time series carries.
pub const MAX_POINTS: usize = 300;

/// What kind of probe a check is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
	Http,
	Tcp,
}

impl Kind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Http => "http",
			Self::Tcp => "tcp",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"http" => Some(Self::Http),
			"tcp" => Some(Self::Tcp),
			_ => None,
		}
	}
}

/// Where a check stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
	/// No verdict yet.
	Unknown,
	Up,
	Down,
}

impl State {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Unknown => "unknown",
			Self::Up => "up",
			Self::Down => "down",
		}
	}

	pub fn parse(s: &str) -> Self {
		match s {
			"up" => Self::Up,
			"down" => Self::Down,
			_ => Self::Unknown,
		}
	}
}

/// What a run changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
	Nothing,
	WentDown,
	WentUp,
}

/// The flap protection, as a pure function: the state, the failure and success streaks after
/// one more run of `ok`.
///
/// A check with no verdict yet is up on its first good run, without a word; a down check
/// needs [`UP_AFTER`] good runs to be up again, so one lucky answer in an outage is not "recovered".
pub fn advance(state: State, fails: i32, oks: i32, ok: bool) -> (State, i32, i32, Step) {
	let (fails, oks) = if ok {
		(0, oks.saturating_add(1))
	} else {
		(fails.saturating_add(1), 0)
	};
	let (state, step) = match state {
		State::Unknown | State::Up if !ok && fails >= DOWN_AFTER => (State::Down, Step::WentDown),
		State::Unknown if ok => (State::Up, Step::Nothing),
		State::Down if ok && oks >= UP_AFTER => (State::Up, Step::WentUp),
		other => (other, Step::Nothing),
	};
	(state, fails, oks, step)
}

/// One check, as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Check {
	pub id: UptimeCheckId,
	pub team_id: TeamId,
	pub application_id: Option<ApplicationId>,
	pub auto_key: Option<String>,
	pub name: String,
	pub kind: String,
	pub url: Option<String>,
	pub method: String,
	pub expected_status: i32,
	pub keyword: Option<String>,
	pub host: Option<String>,
	pub port: Option<i32>,
	pub timeout_secs: i32,
	pub interval_secs: i32,
	pub tls_warn_days: Option<i32>,
	pub enabled: bool,
	pub status_visible: bool,
	pub status_name: Option<String>,
	pub state: String,
	pub state_since: Option<OffsetDateTime>,
	pub fail_streak: i32,
	pub ok_streak: i32,
	pub last_checked_at: Option<OffsetDateTime>,
	pub last_latency_ms: Option<i32>,
	pub last_error: Option<String>,
	pub tls_expires_at: Option<OffsetDateTime>,
	pub tls_checked_at: Option<OffsetDateTime>,
	pub tls_warned: bool,
}

pub const CHECK_COLUMNS: &str = "id, team_id, application_id, auto_key, name, kind, url, method, \
	expected_status, keyword, host, port, timeout_secs, interval_secs, tls_warn_days, enabled, \
	status_visible, status_name, state, state_since, fail_streak, ok_streak, last_checked_at, \
	last_latency_ms, last_error, tls_expires_at, tls_checked_at, tls_warned";

impl Check {
	pub fn kind(&self) -> Kind {
		Kind::parse(&self.kind).unwrap_or(Kind::Http)
	}

	pub fn state(&self) -> State {
		State::parse(&self.state)
	}

	/// What the check points at, for the people who own it (never for the status page).
	pub fn target(&self) -> String {
		match self.kind() {
			Kind::Http => self.url.clone().unwrap_or_default(),
			Kind::Tcp => format!(
				"{}:{}",
				self.host.as_deref().unwrap_or_default(),
				self.port.unwrap_or_default()
			),
		}
	}
}

/// One response time of a check; `ms` is `None` for a failed run, which breaks the line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatencyPoint {
	pub at: i64,
	pub ms: Option<f64>,
}

impl Point for LatencyPoint {
	fn at(&self) -> i64 {
		self.at
	}

	fn merge(run: &[Self]) -> Self {
		let last = &run[run.len() - 1];
		let good: Vec<f64> = run.iter().filter_map(|p| p.ms).collect();
		Self {
			at: last.at,
			ms: (!good.is_empty()).then(|| good.iter().sum::<f64>() / good.len() as f64),
		}
	}
}

/// One UTC day of one check: how many runs, how many were good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayStat {
	pub day: Date,
	pub total: i32,
	pub ok: i32,
}

impl DayStat {
	/// Percentage of good runs, `None` for a day with none.
	pub fn percent(self) -> Option<f64> {
		(self.total > 0).then(|| f64::from(self.ok) * 100.0 / f64::from(self.total))
	}
}

/// Uptime over `days`, as a percentage; `None` with no runs at all.
pub fn uptime_percent(days: &[DayStat]) -> Option<f64> {
	let (ok, total) = days.iter().fold((0i64, 0i64), |(ok, total), d| {
		(ok + i64::from(d.ok), total + i64::from(d.total))
	});
	(total > 0).then(|| ok as f64 * 100.0 / total as f64)
}

/// Folds the raw results of every day that is still whole into `uptime_daily`. Days the
/// 7-day prune has already begun to eat are left as they are: their row is final.
///
/// # Errors
/// The database rejects a write.
pub async fn rollup(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	let first_whole = (now - RAW_RETENTION)
		.date()
		.next_day()
		.unwrap_or(now.date());
	sqlx::query(
		"INSERT INTO uptime_daily (check_id, day, total, ok, avg_latency_ms)
		SELECT check_id, (at AT TIME ZONE 'UTC')::date, COUNT(*)::int,
			(COUNT(*) FILTER (WHERE ok))::int, (AVG(latency_ms) FILTER (WHERE ok))::int
		FROM uptime_results WHERE (at AT TIME ZONE 'UTC')::date >= $1
		GROUP BY check_id, (at AT TIME ZONE 'UTC')::date
		ON CONFLICT (check_id, day) DO UPDATE
		SET total = EXCLUDED.total, ok = EXCLUDED.ok, avg_latency_ms = EXCLUDED.avg_latency_ms",
	)
	.bind(first_whole)
	.execute(db)
	.await?;
	Ok(())
}

/// Drops raw results older than 7 days, daily rows older than 90 days and incidents that
/// ended more than 90 days ago.
///
/// # Errors
/// The database rejects a write.
pub async fn prune(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM uptime_results WHERE at < $1")
		.bind(now - RAW_RETENTION)
		.execute(db)
		.await?;
	let oldest = (now - Duration::days(DAILY_RETENTION_DAYS)).date();
	sqlx::query("DELETE FROM uptime_daily WHERE day < $1")
		.bind(oldest)
		.execute(db)
		.await?;
	sqlx::query("DELETE FROM uptime_incidents WHERE ended_at < $1")
		.bind(now - Duration::days(DAILY_RETENTION_DAYS))
		.execute(db)
		.await?;
	Ok(())
}

/// Hourly housekeeping from the maintenance loop: roll up, then prune. A failure is logged;
/// the next sweep tries again.
pub async fn maintain(db: &PgPool) {
	let now = OffsetDateTime::now_utc();
	if let Err(e) = rollup(db, now).await {
		tracing::warn!("failed to roll up uptime results: {e}");
	}
	if let Err(e) = prune(db, now).await {
		tracing::warn!("failed to prune uptime results: {e}");
	}
}

/// Per-day figures of `checks` for the last `days` days ending today (UTC), oldest first, one
/// entry per check. Recent days come from the raw results (always whole), older ones from the
/// daily rollup.
///
/// # Errors
/// The database rejects the read.
pub async fn day_stats(
	db: &PgPool,
	checks: &[UptimeCheckId],
	days: i64,
	now: OffsetDateTime,
) -> Result<std::collections::HashMap<UptimeCheckId, Vec<DayStat>>, sqlx::Error> {
	let ids: Vec<i64> = checks.iter().map(|c| c.0).collect();
	let today = now.date();
	let first = today - Duration::days(days - 1);
	// Raw results cover every day from here on; the rollup covers the ones before.
	let raw_from = (now - RAW_RETENTION)
		.date()
		.next_day()
		.unwrap_or(today)
		.max(first);
	let rows = sqlx::query_as::<_, (UptimeCheckId, Date, i32, i32)>(
		"SELECT check_id, day, total, ok FROM uptime_daily
			WHERE check_id = ANY($1) AND day >= $2 AND day < $3
		UNION ALL
		SELECT check_id, (at AT TIME ZONE 'UTC')::date, COUNT(*)::int, (COUNT(*) FILTER (WHERE ok))::int
			FROM uptime_results WHERE check_id = ANY($1) AND (at AT TIME ZONE 'UTC')::date >= $3
			GROUP BY check_id, (at AT TIME ZONE 'UTC')::date
		ORDER BY 1, 2",
	)
	.bind(&ids)
	.bind(first)
	.bind(raw_from)
	.fetch_all(db)
	.await?;
	let mut out: std::collections::HashMap<UptimeCheckId, Vec<DayStat>> =
		checks.iter().map(|c| (*c, Vec::new())).collect();
	for (check, day, total, ok) in rows {
		out.entry(check)
			.or_default()
			.push(DayStat { day, total, ok });
	}
	Ok(out)
}
