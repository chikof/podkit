//! Image auto-update: an application deployed from a registry image (`ghcr.io/me/app:latest`)
//! can ask podkit to check its tag on a schedule and, when the tag names a new digest, redeploy
//! through the normal deploy. The health gate, the zero-downtime swap and the automatic
//! rollback are the safety net; a digest whose auto-update failed or was rolled back is held
//! back until the tag moves again, so a bad image is never deployed in a loop.
//!
//! This is what `podman auto-update` does for containers labelled
//! `io.containers.autoupdate=registry`, driven by podkit instead of podman's systemd timer:
//! podkit-agent owns the containers, and a redeploy has to go through podkit's pipeline.
//!
//! - [`rules`] is the pure part: schedule, decision, backoff.
//! - [`engine`] is the check (the agent asks the registry) and the scheduler loop.
//! - [`service`] holds the use cases behind the API and the web UI.
//!
//! Autodeploy freezes (`autodeploy_freezes`) hold an auto-update the same way they hold a push.

pub mod engine;
mod routes;
pub mod rules;
pub mod service;
#[cfg(test)]
mod tests;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use self::rules::{Checked, Interval, Window};
use crate::autodeploy::clip;
use crate::ids::ApplicationId;

pub use routes::router;

/// An application's auto-update row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Row {
	pub application_id: i64,
	pub enabled: bool,
	pub check_interval: String,
	pub window_start_minute: Option<i32>,
	pub window_end_minute: Option<i32>,
	pub utc_offset_minutes: i32,
	pub next_check_at: Option<OffsetDateTime>,
	pub last_checked_at: Option<OffsetDateTime>,
	pub last_result: Option<String>,
	pub last_detail: String,
	pub remote_digest: Option<String>,
	pub deployment_id: Option<i64>,
	pub deployed_digest: Option<String>,
	pub bad_digest: Option<String>,
}

impl Row {
	#[must_use]
	pub fn interval(&self) -> Interval {
		Interval::parse(&self.check_interval).unwrap_or(Interval::Daily)
	}

	#[must_use]
	pub fn window(&self) -> Option<Window> {
		Some(Window {
			start: i64::from(self.window_start_minute?),
			end: i64::from(self.window_end_minute?),
			offset: i64::from(self.utc_offset_minutes),
		})
	}

	#[must_use]
	pub fn last(&self) -> Option<Checked> {
		self.last_result.as_deref().and_then(Checked::from_stored)
	}

	#[must_use]
	pub fn settings(&self) -> Settings {
		Settings {
			enabled: self.enabled,
			interval: self.interval(),
			window: self.window(),
		}
	}
}

/// What a person sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
	pub enabled: bool,
	pub interval: Interval,
	pub window: Option<Window>,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			enabled: false,
			interval: Interval::Daily,
			window: None,
		}
	}
}

const COLUMNS: &str = "application_id, enabled, check_interval, window_start_minute, \
	window_end_minute, utc_offset_minutes, next_check_at, last_checked_at, last_result, \
	last_detail, remote_digest, deployment_id, deployed_digest, bad_digest";

pub async fn get(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<Option<Row>, sqlx::Error> {
	let sql = format!("SELECT {COLUMNS} FROM image_auto_update WHERE application_id = $1");
	sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.fetch_optional(db)
		.await
}

/// Whether the application's deploys carry the auto-update label.
pub async fn enabled(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<bool, sqlx::Error> {
	Ok(sqlx::query_scalar::<_, bool>(
		"SELECT enabled FROM image_auto_update WHERE application_id = $1",
	)
	.bind(app.0)
	.fetch_optional(db)
	.await?
	.unwrap_or(false))
}

fn minutes(v: Option<i64>) -> Option<i32> {
	v.and_then(|m| i32::try_from(m).ok())
}

/// Stores the settings and when the scheduler looks next (`None` while off).
pub async fn save(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	s: &Settings,
	next: Option<OffsetDateTime>,
) -> Result<Row, sqlx::Error> {
	let sql = format!(
		"INSERT INTO image_auto_update (application_id, enabled, check_interval, \
		 window_start_minute, window_end_minute, utc_offset_minutes, next_check_at) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7) \
		 ON CONFLICT (application_id) DO UPDATE SET enabled = $2, check_interval = $3, \
		 window_start_minute = $4, window_end_minute = $5, utc_offset_minutes = $6, \
		 next_check_at = $7, updated_at = NOW() RETURNING {COLUMNS}"
	);
	sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.bind(s.enabled)
		.bind(s.interval.as_str())
		.bind(minutes(s.window.map(|w| w.start)))
		.bind(minutes(s.window.map(|w| w.end)))
		.bind(minutes(s.window.map(|w| w.offset)).unwrap_or(0))
		.bind(next)
		.fetch_one(db)
		.await
}

/// Applications whose next check is due, oldest first.
pub async fn due(db: impl PgExecutor<'_>, now: OffsetDateTime) -> Result<Vec<i64>, sqlx::Error> {
	sqlx::query_scalar::<_, i64>(
		"SELECT application_id FROM image_auto_update WHERE enabled AND next_check_at <= $1 \
		 ORDER BY next_check_at LIMIT 100",
	)
	.bind(now)
	.fetch_all(db)
	.await
}

/// Takes a due check: moves its next look to `until` only for whoever still finds it due, so
/// one pass checks it once, and a check that dies half way is tried again at `until`.
pub async fn claim(
	db: impl PgExecutor<'_>,
	app: i64,
	now: OffsetDateTime,
	until: OffsetDateTime,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query(
		"UPDATE image_auto_update SET next_check_at = $3 \
		 WHERE application_id = $1 AND enabled AND next_check_at <= $2",
	)
	.bind(app)
	.bind(now)
	.bind(until)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}

/// What one check leaves behind. The digests and the deployment are written as given: the
/// caller carries over what did not change.
#[derive(Debug, Clone)]
pub struct Record {
	pub at: OffsetDateTime,
	pub result: Checked,
	pub detail: String,
	/// `None` keeps the schedule as it is (a manual check while off).
	pub next: Option<OffsetDateTime>,
	pub remote_digest: Option<String>,
	pub deployment_id: Option<i64>,
	pub deployed_digest: Option<String>,
	pub bad_digest: Option<String>,
}

pub async fn record(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	r: &Record,
) -> Result<Option<Row>, sqlx::Error> {
	let sql = format!(
		"UPDATE image_auto_update SET last_checked_at = $2, last_result = $3, last_detail = $4, \
		 next_check_at = CASE WHEN enabled THEN COALESCE($5, next_check_at) ELSE NULL END, \
		 remote_digest = $6, deployment_id = $7, deployed_digest = $8, bad_digest = $9, \
		 updated_at = NOW() WHERE application_id = $1 RETURNING {COLUMNS}"
	);
	sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.bind(r.at)
		.bind(r.result.as_str())
		.bind(clip(&r.detail, 500))
		.bind(r.next)
		.bind(r.remote_digest.as_deref().map(|d| clip(d, 140)))
		.bind(r.deployment_id)
		.bind(r.deployed_digest.as_deref().map(|d| clip(d, 140)))
		.bind(r.bad_digest.as_deref().map(|d| clip(d, 140)))
		.fetch_optional(db)
		.await
}
