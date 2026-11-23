//! Push-to-deploy that is safe to leave on: which pushes deploy ([`rules`]), waiting for the
//! commit's checks and for freeze windows ([`engine`]), and the commit status posted back.
//!
//! A push reaches [`engine::handle`] from the application's webhook or the GitHub App's. An
//! application without settings keeps the plain behaviour (a push to the branch it follows
//! deploys at once); the rows here only add rules. The queue is the `autodeploy_pushes` table:
//! a background pass runs only while a push is waiting, so an idle podkit does nothing.

pub mod engine;
pub mod rules;
pub mod service;
#[cfg(test)]
mod tests;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use self::rules::Window;
use crate::error::ServerError;
use crate::ids::ApplicationId;

/// How many patterns, and how long each, an application may carry.
pub const MAX_PATTERNS: usize = 20;
const MAX_PATTERN_LEN: usize = 120;

/// An application's rules. [`Settings::default`] is what an application without a row gets:
/// the plain behaviour, nothing waits, nothing is posted.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per column")]
pub struct Settings {
	pub enabled: bool,
	pub branch_patterns: Vec<String>,
	pub deploy_tags: bool,
	pub path_filters: Vec<String>,
	pub wait_for_ci: bool,
	pub ci_timeout_secs: i32,
	pub deploy_on_ci_timeout: bool,
	pub post_status: bool,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			enabled: true,
			branch_patterns: Vec::new(),
			deploy_tags: false,
			path_filters: Vec::new(),
			wait_for_ci: false,
			ci_timeout_secs: 900,
			deploy_on_ci_timeout: false,
			post_status: false,
		}
	}
}

impl Settings {
	/// What the form starts from when an application has no settings yet.
	#[must_use]
	pub fn form_default() -> Self {
		Self {
			post_status: true,
			..Self::default()
		}
	}

	/// Whether the settings can be stored.
	///
	/// # Errors
	/// [`ServerError::Validation`] naming the field.
	pub fn validate(&self) -> Result<(), ServerError> {
		if !(30..=86_400).contains(&self.ci_timeout_secs) {
			return Err(ServerError::Validation(
				"Wait for checks must be between 30 seconds and 24 hours.".to_owned(),
			));
		}
		check_patterns(&self.branch_patterns, "Branch patterns")?;
		check_patterns(&self.path_filters, "Path filters")
	}
}

fn check_patterns(list: &[String], what: &str) -> Result<(), ServerError> {
	if list.len() > MAX_PATTERNS {
		return Err(ServerError::Validation(format!(
			"{what}: at most {MAX_PATTERNS} lines."
		)));
	}
	for p in list {
		if p.chars().count() > MAX_PATTERN_LEN || p.chars().any(char::is_control) {
			return Err(ServerError::Validation(format!(
				"{what}: a line is too long or has a control character."
			)));
		}
	}
	Ok(())
}

/// A freeze window as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Freeze {
	pub id: i64,
	pub kind: String,
	pub start_minute: Option<i32>,
	pub end_minute: Option<i32>,
	pub utc_offset_minutes: i32,
	pub starts_at: Option<OffsetDateTime>,
	pub ends_at: Option<OffsetDateTime>,
	pub note: String,
}

impl Freeze {
	/// The window the row describes.
	#[must_use]
	pub fn window(&self) -> Option<Window> {
		match (self.kind.as_str(), self.start_minute, self.end_minute) {
			("recurring", Some(start), Some(end)) => Some(Window::Recurring {
				start: i64::from(start),
				end: i64::from(end),
				offset: i64::from(self.utc_offset_minutes),
			}),
			("once", _, _) => Some(Window::Once {
				from: self.starts_at?,
				to: self.ends_at?,
			}),
			_ => None,
		}
	}
}

/// What a push came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
	WaitingCi,
	Frozen,
	Deployed,
	Superseded,
	Skipped,
	Filtered,
	CiFailed,
	CiTimeout,
}

impl State {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::WaitingCi => "waiting_ci",
			Self::Frozen => "frozen",
			Self::Deployed => "deployed",
			Self::Superseded => "superseded",
			Self::Skipped => "skipped",
			Self::Filtered => "filtered",
			Self::CiFailed => "ci_failed",
			Self::CiTimeout => "ci_timeout",
		}
	}

	/// What a page calls it.
	#[must_use]
	pub fn from_stored(raw: &str) -> Option<Self> {
		[
			Self::WaitingCi,
			Self::Frozen,
			Self::Deployed,
			Self::Superseded,
			Self::Skipped,
			Self::Filtered,
			Self::CiFailed,
			Self::CiTimeout,
		]
		.into_iter()
		.find(|s| s.as_str() == raw)
	}

	/// Still in the queue: the background pass has work to do for it.
	#[must_use]
	pub const fn pending(self) -> bool {
		matches!(self, Self::WaitingCi | Self::Frozen)
	}
}

/// One push that reached the rules.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PushRow {
	pub id: i64,
	pub application_id: i64,
	pub git_ref: String,
	pub commit_sha: String,
	pub message: String,
	pub state: String,
	pub detail: String,
	pub deployment_id: Option<i64>,
	pub ci_deadline: Option<OffsetDateTime>,
	pub created_at: OffsetDateTime,
}

impl PushRow {
	#[must_use]
	pub fn state(&self) -> Option<State> {
		State::from_stored(&self.state)
	}
}

const PUSH_COLUMNS: &str = "id, application_id, git_ref, commit_sha, message, state, detail, \
	deployment_id, ci_deadline, created_at";

/// The application's rules, if it has any.
pub async fn settings(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Option<Settings>, sqlx::Error> {
	sqlx::query_as::<_, Settings>(
		"SELECT enabled, branch_patterns, deploy_tags, path_filters, wait_for_ci, ci_timeout_secs, \
		 deploy_on_ci_timeout, post_status FROM autodeploy_settings WHERE application_id = $1",
	)
	.bind(app.0)
	.fetch_optional(db)
	.await
}

pub async fn save_settings(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	s: &Settings,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO autodeploy_settings (application_id, enabled, branch_patterns, deploy_tags, \
		 path_filters, wait_for_ci, ci_timeout_secs, deploy_on_ci_timeout, post_status) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
		 ON CONFLICT (application_id) DO UPDATE SET enabled = $2, branch_patterns = $3, \
		 deploy_tags = $4, path_filters = $5, wait_for_ci = $6, ci_timeout_secs = $7, \
		 deploy_on_ci_timeout = $8, post_status = $9, updated_at = NOW()",
	)
	.bind(app.0)
	.bind(s.enabled)
	.bind(&s.branch_patterns)
	.bind(s.deploy_tags)
	.bind(&s.path_filters)
	.bind(s.wait_for_ci)
	.bind(s.ci_timeout_secs)
	.bind(s.deploy_on_ci_timeout)
	.bind(s.post_status)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn freezes(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<Freeze>, sqlx::Error> {
	sqlx::query_as::<_, Freeze>(
		"SELECT id, kind, start_minute, end_minute, utc_offset_minutes, starts_at, ends_at, note \
		 FROM autodeploy_freezes WHERE application_id = $1 ORDER BY kind DESC, id",
	)
	.bind(app.0)
	.fetch_all(db)
	.await
}

/// A freeze to add.
pub struct NewFreeze {
	pub kind: &'static str,
	pub start_minute: Option<i32>,
	pub end_minute: Option<i32>,
	pub utc_offset_minutes: i32,
	pub starts_at: Option<OffsetDateTime>,
	pub ends_at: Option<OffsetDateTime>,
	pub note: String,
}

pub async fn add_freeze(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	f: &NewFreeze,
	by: crate::ids::UserId,
) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar::<_, i64>(
		"INSERT INTO autodeploy_freezes (application_id, kind, start_minute, end_minute, \
		 utc_offset_minutes, starts_at, ends_at, note, created_by) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
	)
	.bind(app.0)
	.bind(f.kind)
	.bind(f.start_minute)
	.bind(f.end_minute)
	.bind(f.utc_offset_minutes)
	.bind(f.starts_at)
	.bind(f.ends_at)
	.bind(&f.note)
	.bind(by.0)
	.fetch_one(db)
	.await
}

/// Removes a freeze of this application. `false` when there is none.
pub async fn remove_freeze(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	id: i64,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query("DELETE FROM autodeploy_freezes WHERE id = $1 AND application_id = $2")
		.bind(id)
		.bind(app.0)
		.execute(db)
		.await?;
	Ok(done.rows_affected() > 0)
}

/// The windows of every freeze of the application that can be read.
pub async fn windows(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<Window>, sqlx::Error> {
	Ok(freezes(db, app)
		.await?
		.iter()
		.filter_map(Freeze::window)
		.collect())
}

/// What a push is recorded with.
pub struct NewPush<'a> {
	pub git_ref: &'a str,
	pub sha: &'a str,
	pub message: &'a str,
	pub state: State,
	pub detail: &'a str,
	pub ci_deadline: Option<OffsetDateTime>,
}

/// Cuts text to `max` characters for a column.
pub(crate) fn clip(text: &str, max: usize) -> String {
	text.chars().take(max).collect()
}

pub async fn record_push(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	p: &NewPush<'_>,
) -> Result<PushRow, sqlx::Error> {
	let sql = format!(
		"INSERT INTO autodeploy_pushes (application_id, git_ref, commit_sha, message, state, detail, \
		 ci_deadline) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {PUSH_COLUMNS}"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.bind(clip(p.git_ref, 255))
		.bind(clip(p.sha, 64))
		.bind(clip(p.message.lines().next().unwrap_or_default(), 200))
		.bind(p.state.as_str())
		.bind(clip(p.detail, 500))
		.bind(p.ci_deadline)
		.fetch_one(db)
		.await
}

/// Marks every waiting push of the application as replaced. Returns them.
pub async fn supersede_pending(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<PushRow>, sqlx::Error> {
	let sql = format!(
		"UPDATE autodeploy_pushes SET state = 'superseded', detail = 'Replaced by a newer push.', \
		 updated_at = NOW() WHERE application_id = $1 AND state IN ('waiting_ci', 'frozen') \
		 RETURNING {PUSH_COLUMNS}"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.fetch_all(db)
		.await
}

/// Takes a waiting push for release: it becomes `deployed` only for whoever finds it still
/// waiting, so a background pass and a maintainer's force cannot both deploy it.
pub async fn claim(db: impl PgExecutor<'_>, id: i64) -> Result<Option<PushRow>, sqlx::Error> {
	let sql = format!(
		"UPDATE autodeploy_pushes SET state = 'deployed', updated_at = NOW() \
		 WHERE id = $1 AND state IN ('waiting_ci', 'frozen') RETURNING {PUSH_COLUMNS}"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(id)
		.fetch_optional(db)
		.await
}

pub async fn set_state(
	db: impl PgExecutor<'_>,
	id: i64,
	state: State,
	detail: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE autodeploy_pushes SET state = $2, detail = $3, updated_at = NOW() WHERE id = $1",
	)
	.bind(id)
	.bind(state.as_str())
	.bind(clip(detail, 500))
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_detail(db: impl PgExecutor<'_>, id: i64, detail: &str) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE autodeploy_pushes SET detail = $2, updated_at = NOW() WHERE id = $1")
		.bind(id)
		.bind(clip(detail, 500))
		.execute(db)
		.await?;
	Ok(())
}

pub async fn link_deployment(
	db: impl PgExecutor<'_>,
	id: i64,
	deployment: i64,
	detail: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE autodeploy_pushes SET deployment_id = $2, detail = $3, updated_at = NOW() WHERE id = $1",
	)
	.bind(id)
	.bind(deployment)
	.bind(clip(detail, 500))
	.execute(db)
	.await?;
	Ok(())
}

/// Every push in the queue, oldest first.
pub async fn pending(db: impl PgExecutor<'_>) -> Result<Vec<PushRow>, sqlx::Error> {
	let sql = format!(
		"SELECT {PUSH_COLUMNS} FROM autodeploy_pushes WHERE state IN ('waiting_ci', 'frozen') ORDER BY id"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.fetch_all(db)
		.await
}

/// One push of the application.
pub async fn find_push(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	id: i64,
) -> Result<Option<PushRow>, sqlx::Error> {
	let sql = format!(
		"SELECT {PUSH_COLUMNS} FROM autodeploy_pushes WHERE id = $1 AND application_id = $2"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(id)
		.bind(app.0)
		.fetch_optional(db)
		.await
}

/// The newest pushes of the application.
pub async fn recent(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	limit: i64,
) -> Result<Vec<PushRow>, sqlx::Error> {
	let sql = format!(
		"SELECT {PUSH_COLUMNS} FROM autodeploy_pushes WHERE application_id = $1 ORDER BY id DESC LIMIT $2"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.bind(limit)
		.fetch_all(db)
		.await
}

/// The push a deployment was made for.
pub async fn by_deployment(
	db: impl PgExecutor<'_>,
	deployment: i64,
) -> Result<Option<PushRow>, sqlx::Error> {
	let sql = format!(
		"SELECT {PUSH_COLUMNS} FROM autodeploy_pushes WHERE deployment_id = $1 ORDER BY id DESC LIMIT 1"
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(deployment)
		.fetch_optional(db)
		.await
}

/// Pushes whose deployment is queued or still building. A newer
/// release replaces these; one that is already deploying is left to finish.
pub async fn not_started(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<PushRow>, sqlx::Error> {
	let sql = format!(
		"SELECT {} FROM autodeploy_pushes p JOIN deployments d ON d.id = p.deployment_id \
		 WHERE p.application_id = $1 AND p.state = 'deployed' AND d.status IN ('queued', 'building')",
		PUSH_COLUMNS
			.split(", ")
			.map(|c| format!("p.{c}"))
			.collect::<Vec<_>>()
			.join(", ")
	);
	sqlx::query_as::<_, PushRow>(sqlx::AssertSqlSafe(sql))
		.bind(app.0)
		.fetch_all(db)
		.await
}

/// Keeps the newest `keep` finished pushes of the application.
pub async fn prune(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	keep: i64,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"DELETE FROM autodeploy_pushes WHERE application_id = $1 AND state NOT IN ('waiting_ci', 'frozen') \
		 AND id NOT IN (SELECT id FROM autodeploy_pushes WHERE application_id = $1 ORDER BY id DESC LIMIT $2)",
	)
	.bind(app.0)
	.bind(keep)
	.execute(db)
	.await?;
	Ok(())
}
