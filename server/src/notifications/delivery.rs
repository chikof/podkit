//! The delivery queue: claims due rows, sends them, retries with exponential backoff, parks
//! what keeps failing as dead letters, rate-limits each channel and releases the quiet-hours
//! digest. It runs from the jobs dispatcher's loop (see [`wake`]); there is no service of its
//! own.

use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::channel::{CHANNEL_COLUMNS, Channel};
use super::event::Severity;
use super::format::{DigestItem, Message};
use super::{pipeline, send, settings};
use crate::ids::{NotificationChannelId, NotificationDeliveryId, TeamId};
use crate::state::AppState;

/// A delivery that failed this many times is dead.
pub const MAX_ATTEMPTS: i32 = 6;
/// Most messages one channel gets per minute; the rest wait their turn.
pub const RATE_LIMIT_PER_MINUTE: i64 = 20;
/// How long a rate-limited delivery waits before it is tried again.
const RATE_WAIT: Duration = Duration::seconds(15);
/// A delivery left `sending` this long by a crashed process is queued again.
const STUCK_AFTER: Duration = Duration::minutes(5);
const BATCH: i64 = 25;
/// Rows kept after this long: sent and dead deliveries, and old events.
pub const RETENTION_DAYS: i64 = 30;

/// How long to wait before attempt `attempts + 1`: 30 s, 1 min, 2 min, 4 min, 8 min, capped at
/// an hour.
pub fn backoff(attempts: i32) -> Duration {
	let exponent = u32::try_from(attempts.saturating_sub(1).clamp(0, 12)).unwrap_or(0);
	Duration::seconds((30_i64 << exponent).min(3600))
}

/// What one pass did, for the tests and the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
	pub sent: usize,
	pub retried: usize,
	pub dead: usize,
	pub rate_limited: usize,
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Runs one pass in the background unless one is already running. Called on every turn of the
/// jobs dispatcher's loop.
pub fn wake(state: &AppState) {
	if RUNNING.swap(true, Ordering::AcqRel) {
		return;
	}
	let state = state.clone();
	state.jobs.clone().tracker.spawn(async move {
		if let Err(e) = tick(&state, OffsetDateTime::now_utc()).await {
			tracing::warn!("notification delivery pass failed: {e}");
		}
		RUNNING.store(false, Ordering::Release);
	});
}

/// One pass: folds quiet-hours leftovers into digests, follows up on repeats, then sends
/// what is due.
pub async fn tick(state: &AppState, now: OffsetDateTime) -> Result<Report, sqlx::Error> {
	let db = &state.db;
	sqlx::query(
		"UPDATE notification_deliveries SET status = 'queued'
		WHERE status = 'sending' AND next_attempt_at < $1",
	)
	.bind(now - STUCK_AFTER)
	.execute(db)
	.await?;
	release_digests(db, now).await?;
	pipeline::summarize_repeats(db, now).await?;

	let due = sqlx::query_as::<
		_,
		(
			NotificationDeliveryId,
			NotificationChannelId,
			sqlx::types::Json<Message>,
			i32,
		),
	>(
		"UPDATE notification_deliveries SET status = 'sending', next_attempt_at = $1
		WHERE id IN (SELECT id FROM notification_deliveries
			WHERE status = 'queued' AND next_attempt_at <= $1
			ORDER BY next_attempt_at, id LIMIT $2 FOR UPDATE SKIP LOCKED)
		RETURNING id, channel_id, message, attempts",
	)
	.bind(now)
	.bind(BATCH)
	.fetch_all(db)
	.await?;

	let mut report = Report::default();
	for (id, channel_id, message, attempts) in due {
		let channel = load_channel(db, channel_id).await?;
		let Some(channel) = channel.filter(|c| c.enabled) else {
			finish(
				db,
				id,
				"dead",
				attempts,
				Some("the channel was disabled or removed"),
				now,
				now,
			)
			.await?;
			report.dead += 1;
			continue;
		};
		if recently_sent(db, channel_id, now).await? >= RATE_LIMIT_PER_MINUTE {
			sqlx::query(
				"UPDATE notification_deliveries SET status = 'queued', next_attempt_at = $2 WHERE id = $1",
			)
			.bind(id)
			.bind(now + RATE_WAIT)
			.execute(db)
			.await?;
			report.rate_limited += 1;
			continue;
		}
		match send::send(state, &channel, &message.0).await {
			Ok(()) => {
				finish(db, id, "sent", attempts + 1, None, now, now).await?;
				report.sent += 1;
			}
			Err(error) => {
				let attempts = attempts + 1;
				if attempts >= MAX_ATTEMPTS {
					finish(db, id, "dead", attempts, Some(&error), now, now).await?;
					report.dead += 1;
				} else {
					finish(
						db,
						id,
						"queued",
						attempts,
						Some(&error),
						now,
						now + backoff(attempts),
					)
					.await?;
					report.retried += 1;
				}
			}
		}
	}
	Ok(report)
}

async fn finish(
	db: &PgPool,
	id: NotificationDeliveryId,
	status: &str,
	attempts: i32,
	error: Option<&str>,
	now: OffsetDateTime,
	next: OffsetDateTime,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE notification_deliveries
		SET status = $2, attempts = $3, last_error = $4, next_attempt_at = $6,
			sent_at = CASE WHEN $2 = 'sent' THEN $5 ELSE sent_at END
		WHERE id = $1",
	)
	.bind(id)
	.bind(status)
	.bind(attempts)
	.bind(error)
	.bind(now)
	.bind(next)
	.execute(db)
	.await?;
	Ok(())
}

async fn recently_sent(
	db: &PgPool,
	channel: NotificationChannelId,
	now: OffsetDateTime,
) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT COUNT(*) FROM notification_deliveries
		WHERE channel_id = $1 AND status = 'sent' AND sent_at > $2",
	)
	.bind(channel)
	.bind(now - Duration::minutes(1))
	.fetch_one(db)
	.await
}

pub async fn load_channel(
	db: &PgPool,
	id: NotificationChannelId,
) -> Result<Option<Channel>, sqlx::Error> {
	sqlx::query_as::<_, Channel>(sqlx::AssertSqlSafe(format!(
		"SELECT {CHANNEL_COLUMNS} FROM notification_channels WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// Once a team's quiet hours are over, folds what waited into one digest message per channel
/// (a single leftover is just sent as it was).
pub async fn release_digests(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	let waiting = sqlx::query_as::<_, (TeamId, NotificationChannelId)>(
		"SELECT DISTINCT team_id, channel_id FROM notification_deliveries WHERE status = 'deferred'",
	)
	.fetch_all(db)
	.await?;
	for (team, channel) in waiting {
		if settings::load(db, team).await?.is_quiet(now) {
			continue;
		}
		let rows = sqlx::query_as::<_, (NotificationDeliveryId, sqlx::types::Json<Message>)>(
			"SELECT id, message FROM notification_deliveries
			WHERE team_id = $1 AND channel_id = $2 AND status = 'deferred' ORDER BY id LIMIT 500",
		)
		.bind(team)
		.bind(channel)
		.fetch_all(db)
		.await?;
		let ids: Vec<i64> = rows.iter().map(|(id, _)| id.0).collect();
		if let [(id, _)] = rows.as_slice() {
			sqlx::query(
				"UPDATE notification_deliveries SET status = 'queued', next_attempt_at = $2 WHERE id = $1",
			)
			.bind(id)
			.bind(now)
			.execute(db)
			.await?;
			continue;
		}
		let items: Vec<DigestItem> = rows
			.iter()
			.map(|(_, m)| DigestItem {
				title: m.0.title.clone(),
				severity: m.0.severity,
				at: m.0.at.clone(),
				count: m.0.count,
			})
			.collect();
		let worst = items
			.iter()
			.map(|i| i.severity)
			.max()
			.unwrap_or(Severity::Info);
		let digest = Message {
			kind: "digest".to_owned(),
			severity: worst,
			title: format!("{} notifications while you were away", items.len()),
			body: String::new(),
			link: Some(format!("/teams/{team}/notifications")),
			count: 1,
			at: now
				.format(&time::format_description::well_known::Rfc3339)
				.unwrap_or_default(),
			team_id: team.0,
			project_id: None,
			environment_id: None,
			event_id: None,
			digest: items,
			test: false,
		};
		let mut tx = db.begin().await?;
		sqlx::query(
			"INSERT INTO notification_deliveries
				(id, team_id, channel_id, message, status, next_attempt_at, created_at)
			VALUES ($1, $2, $3, $4, 'queued', $5, $5)",
		)
		.bind(NotificationDeliveryId::generate())
		.bind(team)
		.bind(channel)
		.bind(serde_json::to_value(&digest).unwrap_or_default())
		.bind(now)
		.execute(&mut *tx)
		.await?;
		sqlx::query("UPDATE notification_deliveries SET status = 'digested' WHERE id = ANY($1)")
			.bind(&ids)
			.execute(&mut *tx)
			.await?;
		tx.commit().await?;
	}
	Ok(())
}

/// Drops what is older than [`RETENTION_DAYS`].
pub async fn purge(db: &PgPool) -> Result<(), sqlx::Error> {
	let days = RETENTION_DAYS;
	sqlx::query(
		"DELETE FROM notification_deliveries
		WHERE status IN ('sent', 'dead', 'digested') AND created_at < NOW() - make_interval(days => $1::int)",
	)
	.bind(i32::try_from(days).unwrap_or(30))
	.execute(db)
	.await?;
	sqlx::query(
		"DELETE FROM notification_events WHERE last_at < NOW() - make_interval(days => $1::int)",
	)
	.bind(i32::try_from(days * 3).unwrap_or(90))
	.execute(db)
	.await?;
	sqlx::query("DELETE FROM notification_flaps WHERE last_change < NOW() - INTERVAL '1 day'")
		.execute(db)
		.await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn backoff_doubles_up_to_an_hour() {
		let secs: Vec<i64> = (1..=9).map(|n| backoff(n).whole_seconds()).collect();
		assert_eq!(secs, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600]);
	}
}
