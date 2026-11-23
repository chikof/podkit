//! Previews of pull requests: a temporary copy of an application per open pull request.
//!
//! A pull request opened or updated at the provider reaches [`service::handle`] from the
//! provider's webhook. If the application has previews on, the pull request gets its own
//! environment (made by the environment clone machinery, without data), a fresh empty
//! database of every engine the application uses, and the application built from the pull
//! request's head. A job ([`run`]) brings it up, runs the post-deploy command in it, and
//! writes the link into the pull request and onto the commit. Closing or merging the pull
//! request, or a week without activity, removes everything again.
//!
//! What a preview gets and what it must never get is decided in [`plan`], which is pure:
//! - fresh databases with new credentials, never a dump of the application's;
//! - only the variables marked "available to previews", and none at all for a pull request
//!   from a fork that nobody with write access approved (`/preview`);
//! - no user network, no model link: a preview shares nothing with production but the host.

#![allow(
	dead_code,
	reason = "the previews section of the Git tab and the variable toggle (still to build) read the settings, the view and the scoped lookups"
)]

pub mod build;
pub mod comment;
pub mod event;
pub mod plan;
pub mod policy;
pub mod run;
pub mod service;

#[cfg(test)]
mod tests;

use serde::Serialize;
use sqlx::PgExecutor;
use time::OffsetDateTime;

pub use crate::ids::PreviewId;
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, TeamId};

/// Open previews an application may have unless it says otherwise.
pub const DEFAULT_MAX: i32 = 3;
/// Days without activity before a preview is removed, unless the application says otherwise.
pub const DEFAULT_TTL_DAYS: i32 = 7;

/// How far a preview is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
	/// A fork's pull request: a maintainer must comment `/preview`.
	AwaitingApproval,
	/// The application's limit of open previews is reached.
	Limited,
	Queued,
	Building,
	Ready,
	Failed,
	/// The job is taking it down.
	Removing,
}

impl State {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::AwaitingApproval => "awaiting_approval",
			Self::Limited => "limited",
			Self::Queued => "queued",
			Self::Building => "building",
			Self::Ready => "ready",
			Self::Failed => "failed",
			Self::Removing => "removing",
		}
	}

	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		[
			Self::AwaitingApproval,
			Self::Limited,
			Self::Queued,
			Self::Building,
			Self::Ready,
			Self::Failed,
			Self::Removing,
		]
		.into_iter()
		.find(|v| v.as_str() == s)
	}

	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::AwaitingApproval => "Waiting for approval",
			Self::Limited => "Waiting for a free place",
			Self::Queued => "Queued",
			Self::Building => "Building",
			Self::Ready => "Ready",
			Self::Failed => "Failed",
			Self::Removing => "Removing",
		}
	}
}

/// What an application says about its previews. [`Settings::default`] is what an application
/// without a row gets: previews off.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per column")]
pub struct Settings {
	pub enabled: bool,
	/// Run in the preview's container once it is up.
	pub command: String,
	pub max_concurrent: i32,
	pub ttl_days: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
	/// Whether an approved pull request from a fork gets the variables marked for previews.
	pub fork_variables: bool,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			enabled: false,
			command: String::new(),
			max_concurrent: DEFAULT_MAX,
			ttl_days: DEFAULT_TTL_DAYS,
			memory_limit_mb: None,
			cpu_limit: None,
			fork_variables: false,
		}
	}
}

/// Row from `previews`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Preview {
	pub id: PreviewId,
	pub team_id: TeamId,
	pub project_id: ProjectId,
	pub application_id: ApplicationId,
	pub number: i32,
	pub title: String,
	pub head_sha: String,
	pub head_ref: String,
	pub head_repo: String,
	pub from_fork: bool,
	pub approved: bool,
	pub state: String,
	pub detail: String,
	pub environment_id: Option<EnvironmentId>,
	#[allow(clippy::struct_field_names, reason = "the column's name")]
	pub preview_application_id: Option<ApplicationId>,
	pub url: String,
	pub comment_id: Option<String>,
	pub last_activity_at: OffsetDateTime,
	pub created_at: OffsetDateTime,
}

impl Preview {
	/// The state, or [`State::Failed`] for a value this build does not know.
	#[must_use]
	pub fn state(&self) -> State {
		State::parse(&self.state).unwrap_or(State::Failed)
	}
}

const COLUMNS: &str = "id, team_id, project_id, application_id, number, title, head_sha, head_ref,
	head_repo, from_fork, approved, state, detail, environment_id, preview_application_id, url,
	comment_id, last_activity_at, created_at";

const SETTINGS_COLUMNS: &str =
	"enabled, command, max_concurrent, ttl_days, memory_limit_mb, cpu_limit, fork_variables";

/// An application's settings; `None` when it never saved any.
pub async fn settings(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Option<Settings>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {SETTINGS_COLUMNS} FROM preview_settings WHERE application_id = $1"
	)))
	.bind(app)
	.fetch_optional(db)
	.await
}

pub async fn save_settings(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	s: &Settings,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO preview_settings
			(application_id, enabled, command, max_concurrent, ttl_days, memory_limit_mb,
			 cpu_limit, fork_variables)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
		ON CONFLICT (application_id) DO UPDATE SET
			enabled = EXCLUDED.enabled, command = EXCLUDED.command,
			max_concurrent = EXCLUDED.max_concurrent, ttl_days = EXCLUDED.ttl_days,
			memory_limit_mb = EXCLUDED.memory_limit_mb, cpu_limit = EXCLUDED.cpu_limit,
			fork_variables = EXCLUDED.fork_variables, updated_at = NOW()",
	)
	.bind(app)
	.bind(s.enabled)
	.bind(&s.command)
	.bind(s.max_concurrent)
	.bind(s.ttl_days)
	.bind(s.memory_limit_mb)
	.bind(s.cpu_limit)
	.bind(s.fork_variables)
	.execute(db)
	.await?;
	Ok(())
}

/// Lookup by id alone, with no team check. Only for the job and the sweep; handlers use
/// [`service`].
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: PreviewId,
) -> Result<Option<Preview>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM previews WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// Lookup scoped to the team and project, so a preview of another team never resolves.
pub async fn find_scoped(
	db: impl PgExecutor<'_>,
	team: TeamId,
	project: ProjectId,
	id: PreviewId,
) -> Result<Option<Preview>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM previews WHERE id = $1 AND team_id = $2 AND project_id = $3"
	)))
	.bind(id)
	.bind(team)
	.bind(project)
	.fetch_optional(db)
	.await
}

/// The preview of pull request `number` of `app`.
pub async fn find_for_pr(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	number: i32,
) -> Result<Option<Preview>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM previews WHERE application_id = $1 AND number = $2"
	)))
	.bind(app)
	.bind(number)
	.fetch_optional(db)
	.await
}

/// The previews of `app`, newest pull request first.
pub async fn list_for_app(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<Preview>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM previews WHERE application_id = $1 ORDER BY number DESC"
	)))
	.bind(app)
	.fetch_all(db)
	.await
}

/// The preview whose environment is `environment`, if it is one.
pub async fn find_for_environment(
	db: impl PgExecutor<'_>,
	environment: EnvironmentId,
) -> Result<Option<Preview>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM previews WHERE environment_id = $1"
	)))
	.bind(environment)
	.fetch_optional(db)
	.await
}

/// How many of `app`'s previews hold an environment, whatever state they are in.
pub async fn live_count(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT COUNT(*) FROM previews WHERE application_id = $1 AND environment_id IS NOT NULL",
	)
	.bind(app)
	.fetch_one(db)
	.await
}

pub async fn set_state(
	db: impl PgExecutor<'_>,
	id: PreviewId,
	state: State,
	detail: &str,
) -> Result<(), sqlx::Error> {
	let detail: String = detail.chars().take(500).collect();
	sqlx::query("UPDATE previews SET state = $2, detail = $3 WHERE id = $1")
		.bind(id)
		.bind(state.as_str())
		.bind(detail)
		.execute(db)
		.await?;
	Ok(())
}

pub async fn set_comment(
	db: impl PgExecutor<'_>,
	id: PreviewId,
	comment: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE previews SET comment_id = $2 WHERE id = $1")
		.bind(id)
		.bind(comment)
		.execute(db)
		.await?;
	Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, id: PreviewId) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM previews WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

/// Marks an application variable as available (or not) to previews. The caller scoped the
/// application.
pub async fn set_var_preview(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	var: crate::ids::EnvVarId,
	available: bool,
) -> Result<bool, sqlx::Error> {
	let done =
		sqlx::query("UPDATE env_vars SET preview = $3 WHERE id = $2 AND application_id = $1")
			.bind(app)
			.bind(var)
			.bind(available)
			.execute(db)
			.await?;
	Ok(done.rows_affected() > 0)
}

/// The keys of `app`'s variables that are available to previews.
pub async fn preview_keys(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
	let keys: Vec<String> =
		sqlx::query_scalar("SELECT key FROM env_vars WHERE application_id = $1 AND preview")
			.bind(app)
			.fetch_all(db)
			.await?;
	Ok(keys.into_iter().collect())
}
