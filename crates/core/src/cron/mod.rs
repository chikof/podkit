//! Scheduled tasks: a command run on a cron schedule, either inside a running
//! application's container (`exec`) or in a one-shot container (`container`).
//!
//! A task lives in an environment. Each run is a `cron_runs` row and a job of kind
//! `cron_run`, so it gets the job queue's claim and cancel and the job log. The scheduler
//! ([`scheduler`]) works from the server's job loop; the use cases are in [`service`].

mod routes;
pub mod run;
pub mod schedule;
pub mod scheduler;
pub mod service;
pub mod store;

/// The columns of a task, as a literal so a query can `concat!` them.
macro_rules! task_cols {
	() => {
		"id, project_id, environment_id, server_id, application_id, name, kind, image, command, \
		schedule, timezone, timeout_s, concurrency, max_retries, retry_backoff_s, enabled, \
		next_run_at, last_outcome"
	};
}

/// The columns of a run.
macro_rules! run_cols {
	() => {
		"id, task_id, job_id, status, trigger, attempt, scheduled_for, created_at, started_at, \
		finished_at, exit_code, error"
	};
}

use run_cols;
pub(crate) use task_cols;

#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{ApplicationId, CronRunId, CronTaskId, EnvironmentId, JobId, ProjectId, ServerId};

pub use routes::router;

/// What a task runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Kind {
	/// Inside the application's running container.
	Exec,
	/// In a new container that is removed afterwards.
	Container,
}

impl Kind {
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Exec => "exec",
			Self::Container => "container",
		}
	}

	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		[Self::Exec, Self::Container]
			.into_iter()
			.find(|k| k.as_str() == s)
	}

	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Exec => "Run in the app",
			Self::Container => "One-shot container",
		}
	}
}

/// What happens when a run is due while the previous one is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Concurrency {
	/// The new run is recorded as skipped.
	Skip,
	/// The new run waits its turn.
	Queue,
	/// Runs overlap.
	Allow,
}

impl Concurrency {
	pub const ALL: [Self; 3] = [Self::Skip, Self::Queue, Self::Allow];

	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Skip => "skip",
			Self::Queue => "queue",
			Self::Allow => "allow",
		}
	}

	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|c| c.as_str() == s)
	}

	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Skip => "Skip it",
			Self::Queue => "Queue it",
			Self::Allow => "Run both",
		}
	}
}

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
	Queued,
	Running,
	Succeeded,
	Failed,
	Cancelled,
	/// Not run: the concurrency policy said skip.
	Skipped,
}

impl RunStatus {
	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Queued => "Queued",
			Self::Running => "Running",
			Self::Succeeded => "Succeeded",
			Self::Failed => "Failed",
			Self::Cancelled => "Cancelled",
			Self::Skipped => "Skipped",
		}
	}

	/// Still queued or running.
	#[must_use]
	pub fn is_active(self) -> bool {
		matches!(self, Self::Queued | Self::Running)
	}
}

/// Why a run exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
	/// Its slot of the schedule came up.
	Schedule,
	/// Its slot passed while podkit was down; run once when it came back.
	CatchUp,
	/// Someone pressed Run now.
	Manual,
	/// An earlier attempt failed.
	Retry,
}

impl Trigger {
	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Schedule => "Scheduled",
			Self::CatchUp => "Catch-up",
			Self::Manual => "Run now",
			Self::Retry => "Retry",
		}
	}
}

/// A row of `cron_tasks`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CronTask {
	pub id: CronTaskId,
	pub project_id: ProjectId,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub application_id: Option<ApplicationId>,
	pub name: String,
	pub kind: Kind,
	pub image: Option<String>,
	pub command: String,
	pub schedule: String,
	pub timezone: String,
	pub timeout_s: i32,
	pub concurrency: Concurrency,
	pub max_retries: i32,
	pub retry_backoff_s: i32,
	pub enabled: bool,
	pub next_run_at: Option<OffsetDateTime>,
	pub last_outcome: Option<String>,
}

/// A row of `cron_runs`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CronRun {
	pub id: CronRunId,
	pub task_id: CronTaskId,
	pub job_id: Option<JobId>,
	pub status: RunStatus,
	pub trigger: Trigger,
	pub attempt: i32,
	pub scheduled_for: Option<OffsetDateTime>,
	pub created_at: OffsetDateTime,
	pub started_at: Option<OffsetDateTime>,
	pub finished_at: Option<OffsetDateTime>,
	pub exit_code: Option<i32>,
	pub error: Option<String>,
}

/// Limits of the fields a person sets.
pub mod limits {
	pub const MAX_NAME: usize = 100;
	pub const MAX_COMMAND: usize = 4000;
	pub const MAX_TIMEOUT_S: i32 = 86_400;
	pub const MAX_RETRIES: i32 = 10;
	pub const MAX_BACKOFF_S: i32 = 3600;
	pub const DEFAULT_TIMEOUT_S: i32 = 300;
	pub const DEFAULT_BACKOFF_S: i32 = 60;
}
