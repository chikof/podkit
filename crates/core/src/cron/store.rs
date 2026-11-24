//! Queries over `cron_tasks` and `cron_runs`. Nothing here checks who is asking; the
//! use cases in [`super::service`] do, through [`crate::scope`].

use std::collections::HashMap;

use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

use super::{Concurrency, CronRun, CronTask, Kind, RunStatus, Trigger};
use super::{run_cols, task_cols};
use crate::ids::{ApplicationId, CronRunId, CronTaskId, EnvironmentId, JobId, ProjectId, ServerId};
use crate::jobs::JobKind;

/// A task to insert.
#[derive(Debug, Clone)]
pub struct NewTask {
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
}

/// # Errors
/// A database error, among them the unique violation of the name.
pub async fn insert_task(db: impl PgExecutor<'_>, new: &NewTask) -> Result<CronTask, sqlx::Error> {
	sqlx::query_as(concat!(
		"INSERT INTO cron_tasks (id, project_id, environment_id, server_id, application_id, name, kind,
			image, command, schedule, timezone, timeout_s, concurrency, max_retries, retry_backoff_s,
			enabled, next_run_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
		RETURNING ",
		task_cols!()
	))
	.bind(CronTaskId::generate())
	.bind(new.project_id)
	.bind(new.environment_id)
	.bind(new.server_id)
	.bind(new.application_id)
	.bind(&new.name)
	.bind(new.kind)
	.bind(&new.image)
	.bind(&new.command)
	.bind(&new.schedule)
	.bind(&new.timezone)
	.bind(new.timeout_s)
	.bind(new.concurrency)
	.bind(new.max_retries)
	.bind(new.retry_backoff_s)
	.bind(new.enabled)
	.bind(new.next_run_at)
	.fetch_one(db)
	.await
}

/// Writes back what a person can edit.
///
/// # Errors
/// A database error.
pub async fn update_task(
	db: impl PgExecutor<'_>,
	task: &CronTask,
) -> Result<CronTask, sqlx::Error> {
	sqlx::query_as(concat!(
		"UPDATE cron_tasks SET server_id = $2, application_id = $3, name = $4, image = $5,
			command = $6, schedule = $7, timezone = $8, timeout_s = $9, concurrency = $10,
			max_retries = $11, retry_backoff_s = $12, enabled = $13, next_run_at = $14,
			updated_at = NOW()
		WHERE id = $1 RETURNING ",
		task_cols!()
	))
	.bind(task.id)
	.bind(task.server_id)
	.bind(task.application_id)
	.bind(&task.name)
	.bind(&task.image)
	.bind(&task.command)
	.bind(&task.schedule)
	.bind(&task.timezone)
	.bind(task.timeout_s)
	.bind(task.concurrency)
	.bind(task.max_retries)
	.bind(task.retry_backoff_s)
	.bind(task.enabled)
	.bind(task.next_run_at)
	.fetch_one(db)
	.await
}

/// The task, wherever it is. For jobs and the scheduler, which have no team; a person's
/// request goes through [`crate::scope::cron_task`].
///
/// # Errors
/// A database error.
pub async fn task_by_id(
	db: impl PgExecutor<'_>,
	id: CronTaskId,
) -> Result<Option<CronTask>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		task_cols!(),
		" FROM cron_tasks WHERE id = $1"
	))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// # Errors
/// A database error.
pub async fn delete_task(db: impl PgExecutor<'_>, id: CronTaskId) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM cron_tasks WHERE id = $1")
		.bind(id)
		.execute(db)
		.await
		.map(drop)
}

/// A project's tasks, by environment then name; narrowed to one environment or application.
///
/// # Errors
/// A database error.
pub async fn list_tasks(
	db: impl PgExecutor<'_>,
	project: ProjectId,
	environment: Option<EnvironmentId>,
	application: Option<ApplicationId>,
) -> Result<Vec<CronTask>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		task_cols!(),
		" FROM cron_tasks
		WHERE project_id = $1 AND ($2::BIGINT IS NULL OR environment_id = $2)
			AND ($3::BIGINT IS NULL OR application_id = $3)
		ORDER BY environment_id, name"
	))
	.bind(project)
	.bind(environment)
	.bind(application)
	.fetch_all(db)
	.await
}

/// The newest run of each task.
///
/// # Errors
/// A database error.
pub async fn latest_runs(
	db: impl PgExecutor<'_>,
	tasks: &[CronTaskId],
) -> Result<HashMap<CronTaskId, CronRun>, sqlx::Error> {
	let ids: Vec<i64> = tasks.iter().map(|t| t.0).collect();
	let rows: Vec<CronRun> = sqlx::query_as(concat!(
		"SELECT DISTINCT ON (task_id) ",
		run_cols!(),
		" FROM cron_runs WHERE task_id = ANY($1) ORDER BY task_id, id DESC"
	))
	.bind(ids)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(|r| (r.task_id, r)).collect())
}

/// # Errors
/// A database error.
pub async fn run_by_id(
	db: impl PgExecutor<'_>,
	id: CronRunId,
) -> Result<Option<CronRun>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		run_cols!(),
		" FROM cron_runs WHERE id = $1"
	))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// A task's runs, newest first, `limit` at a time, older than `before`.
///
/// # Errors
/// A database error.
pub async fn list_runs(
	db: impl PgExecutor<'_>,
	task: CronTaskId,
	before: Option<CronRunId>,
	limit: i64,
) -> Result<Vec<CronRun>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		run_cols!(),
		" FROM cron_runs
		WHERE task_id = $1 AND ($2::BIGINT IS NULL OR id < $2)
		ORDER BY id DESC LIMIT $3"
	))
	.bind(task)
	.bind(before)
	.bind(limit)
	.fetch_all(db)
	.await
}

/// Runs of the task that are queued or running, and how many of them are queued.
///
/// # Errors
/// A database error.
pub async fn active_counts(
	db: impl PgExecutor<'_>,
	task: CronTaskId,
) -> Result<(i64, i64), sqlx::Error> {
	sqlx::query_as(
		"SELECT COUNT(*), COUNT(*) FILTER (WHERE status = 'queued') FROM cron_runs
		WHERE task_id = $1 AND status IN ('queued', 'running')",
	)
	.bind(task)
	.fetch_one(db)
	.await
}

/// Queues a run and its job. `run_after` delays the job (a retry's backoff).
///
/// # Errors
/// A database error.
pub async fn enqueue_run(
	conn: &mut PgConnection,
	task: &CronTask,
	trigger: Trigger,
	attempt: i32,
	scheduled_for: Option<OffsetDateTime>,
	run_after: OffsetDateTime,
) -> Result<CronRun, sqlx::Error> {
	let run = CronRunId::generate();
	let job = JobId::generate();
	// Runs sharing a lock key never run at the same time: that is what `queue` means.
	let lock = match task.concurrency {
		Concurrency::Allow => format!("cron-run:{run}"),
		Concurrency::Skip | Concurrency::Queue => format!("cron:{}", task.id),
	};
	sqlx::query(
		"INSERT INTO jobs (id, kind, server_id, target_id, lock_key, run_after)
		VALUES ($1, $2, $3, $4, $5, $6)",
	)
	.bind(job)
	.bind(JobKind::CronRun)
	.bind(task.server_id)
	.bind(run)
	.bind(lock)
	.bind(run_after)
	.execute(&mut *conn)
	.await?;
	sqlx::query_as(concat!(
		"INSERT INTO cron_runs (id, task_id, job_id, trigger, attempt, scheduled_for)
		VALUES ($1, $2, $3, $4, $5, $6) RETURNING ",
		run_cols!()
	))
	.bind(run)
	.bind(task.id)
	.bind(job)
	.bind(trigger)
	.bind(attempt)
	.bind(scheduled_for)
	.fetch_one(conn)
	.await
}

/// Records a run that did not happen.
///
/// # Errors
/// A database error.
pub async fn insert_skipped(
	conn: &mut PgConnection,
	task: &CronTask,
	trigger: Trigger,
	scheduled_for: Option<OffsetDateTime>,
	why: &str,
) -> Result<CronRun, sqlx::Error> {
	sqlx::query_as(concat!(
		"INSERT INTO cron_runs (id, task_id, status, trigger, scheduled_for, finished_at, error)
		VALUES ($1, $2, 'skipped', $3, $4, NOW(), $5) RETURNING ",
		run_cols!()
	))
	.bind(CronRunId::generate())
	.bind(task.id)
	.bind(trigger)
	.bind(scheduled_for)
	.bind(why)
	.fetch_one(conn)
	.await
}

/// The run has been picked up.
///
/// # Errors
/// A database error.
pub async fn mark_started(db: impl PgExecutor<'_>, id: CronRunId) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE cron_runs SET status = 'running', started_at = NOW()
		WHERE id = $1 AND status = 'queued'",
	)
	.bind(id)
	.execute(db)
	.await
	.map(drop)
}

/// Ends a run that is still queued or running; a run that already ended is left alone.
///
/// # Errors
/// A database error.
pub async fn mark_finished(
	db: impl PgExecutor<'_>,
	id: CronRunId,
	status: RunStatus,
	exit_code: Option<i32>,
	error: Option<&str>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
	sqlx::query(
		"UPDATE cron_runs SET status = $2, finished_at = NOW(),
			exit_code = COALESCE($3, exit_code), error = $4
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(status)
	.bind(exit_code)
	.bind(error)
	.execute(db)
	.await
}
