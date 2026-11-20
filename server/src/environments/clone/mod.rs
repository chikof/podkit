//! Cloning an environment: "copy production into staging".
//!
//! [`service::clone`] copies the configuration of every resource of the source environment
//! into a new environment of the same project, in one transaction, so the new environment
//! is either all there or not there at all. What takes time afterwards is a job
//! ([`run`]): provisioning the cloned databases, copying their data and the volumes
//! (through the backup machinery: a dump or an archive into the chosen backup destination,
//! restored into the clone), and deploying. Every step of the job is a line of the
//! clone's report, so a partial failure says what worked and what did not.
//!
//! What a clone copies and what it leaves out is decided in [`plan`], which is pure and
//! is also what the preview in the UI shows:
//! - names, slugs and so default domains get the new environment's slug as a suffix
//!   (`shop` becomes `shop-staging`), because names and slugs are unique per project or
//!   per server;
//! - custom domains are never copied: a hostname belongs to one application;
//! - variables are copied as written, or with their values blanked ("secrets blank");
//! - databases get new credentials and no public port;
//! - backup schedules, webhooks and git hooks are not copied: they belong to the source.

pub mod copy;
pub mod plan;
mod routes;
pub mod run;
pub mod service;

pub use routes::router;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use sqlx::types::Json;
use time::OffsetDateTime;

use crate::ids::{BackupTargetId, CloneId, EnvironmentId, JobId, ProjectId, TeamId, UserId};
use crate::jobs::JobFailure;
use crate::state::AppState;

/// What to do with variables that may hold secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Secrets {
	/// Copy every value as it is.
	#[default]
	Copy,
	/// Keep the names, empty the values; drop stored git and registry credentials.
	Blank,
}

/// What a clone is asked to do beyond copying configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
	#[serde(default)]
	pub secrets: Secrets,
	/// Copy the data: database dumps and volume contents.
	#[serde(default)]
	pub data: bool,
	/// The backup destination the data goes through; needed with `data`.
	#[serde(default)]
	pub backup_target_id: Option<BackupTargetId>,
	/// Deploy the applications and stacks and start the models once copied.
	#[serde(default)]
	pub deploy: bool,
}

impl Default for Options {
	fn default() -> Self {
		Self {
			secrets: Secrets::Copy,
			data: false,
			backup_target_id: None,
			deploy: false,
		}
	}
}

/// How far a clone is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CloneStatus {
	Queued,
	Running,
	Succeeded,
	/// The environment exists; at least one step failed and the report says which.
	Partial,
	Failed,
}

impl CloneStatus {
	#[must_use]
	pub const fn is_final(self) -> bool {
		matches!(self, Self::Succeeded | Self::Partial | Self::Failed)
	}

	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Queued => "Queued",
			Self::Running => "Running",
			Self::Succeeded => "Done",
			Self::Partial => "Done with problems",
			Self::Failed => "Failed",
		}
	}
}

/// State of one step of a clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
	Pending,
	Running,
	Ok,
	Failed,
	Skipped,
}

/// One line of a clone's report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
	pub label: String,
	pub status: StepStatus,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub detail: Option<String>,
}

impl Step {
	pub fn pending(label: impl Into<String>) -> Self {
		Self {
			label: label.into(),
			status: StepStatus::Pending,
			detail: None,
		}
	}
}

/// A resource of the source and the resource made from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
	pub kind: Kind,
	pub source: i64,
	pub id: i64,
}

/// The kinds of resource a clone copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
	Application,
	Database,
	Stack,
	Model,
}

impl Kind {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Application => "Application",
			Self::Database => "Database",
			Self::Stack => "Stack",
			Self::Model => "Model",
		}
	}
}

/// Row from `environment_clones`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnvironmentClone {
	pub id: CloneId,
	pub team_id: TeamId,
	pub project_id: ProjectId,
	pub source_name: String,
	pub environment_id: Option<EnvironmentId>,
	pub name: String,
	pub slug: String,
	pub options: Json<Options>,
	pub status: CloneStatus,
	pub report: Json<Vec<Step>>,
	pub pairs: Json<Vec<Pair>>,
	pub job_id: Option<JobId>,
	pub created_by: Option<UserId>,
	pub created_at: OffsetDateTime,
	pub finished_at: Option<OffsetDateTime>,
}

const COLUMNS: &str = "id, team_id, project_id, source_name, environment_id,
	name, slug, options, status, report, pairs, job_id, created_by, created_at, finished_at";

/// Lookup by id alone, with no team check. Only for the job; handlers use
/// [`service::get`].
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: CloneId,
) -> Result<Option<EnvironmentClone>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM environment_clones WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// Lookup scoped to the team and project, so a clone of another team never resolves.
pub async fn find_scoped(
	db: impl PgExecutor<'_>,
	team: TeamId,
	project: ProjectId,
	id: CloneId,
) -> Result<Option<EnvironmentClone>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM environment_clones
		WHERE id = $1 AND team_id = $2 AND project_id = $3"
	)))
	.bind(id)
	.bind(team)
	.bind(project)
	.fetch_optional(db)
	.await
}

pub(crate) async fn save_report(
	db: impl PgExecutor<'_>,
	id: CloneId,
	status: CloneStatus,
	report: &[Step],
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE environment_clones
		SET status = $2, report = $3,
			finished_at = CASE WHEN $4 THEN NOW() ELSE finished_at END
		WHERE id = $1",
	)
	.bind(id)
	.bind(status)
	.bind(Json(report))
	.bind(status.is_final())
	.execute(db)
	.await?;
	Ok(())
}

/// Reflects a failed or cancelled job onto the clone: what was not done is skipped.
/// A clone that already finished keeps its report.
pub async fn fail_clone(
	state: &AppState,
	target_id: i64,
	failure: &JobFailure,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
	let message = match failure {
		JobFailure::Cancelled => "cancelled".to_owned(),
		JobFailure::Failed(m) => m.clone(),
	};
	let Some(clone) = find_by_id(&state.db, CloneId(target_id)).await? else {
		return Ok(sqlx::postgres::PgQueryResult::default());
	};
	if clone.status.is_final() {
		return Ok(sqlx::postgres::PgQueryResult::default());
	}
	let mut report = clone.report.0;
	for step in &mut report {
		if matches!(step.status, StepStatus::Pending | StepStatus::Running) {
			step.status = StepStatus::Skipped;
			step.detail = Some(message.clone());
		}
	}
	sqlx::query(
		"UPDATE environment_clones SET status = 'failed', report = $2, finished_at = NOW()
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(clone.id)
	.bind(Json(&report))
	.execute(&state.db)
	.await
}
