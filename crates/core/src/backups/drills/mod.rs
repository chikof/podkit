//! Restore drills: proof that a database's backups come back. A drill takes the newest
//! succeeded backup, has the server's agent restore it into a throwaway container of the
//! same engine and version (no network, no published port, no named volume, resource
//! limits, removed afterwards whatever happens; see `agent/src/drill.rs`), counts the
//! tables of the copy and compares them with the count recorded when the backup was taken.
//! The real database, its volume and its networks are never touched.
//!
//! Drills run on the database's own cron schedule (off by default, see [`schedule`]) or on
//! demand, as `RestoreDrill` jobs locked per database (`drill:{id}`), so a drill never
//! waits for a backup and two drills of one database never overlap. A failed drill raises
//! a `restore_drill_failed` notification.

pub mod routes;
pub mod schedule;
pub mod service;
#[cfg(test)]
mod tests;

use proto::{DrillRequest, Engine, Event, Stream as LogStream};
use sqlx::PgExecutor;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{BackupStatus, call, db_failure, failed, s3};
use crate::databases::{self, Database, DatabaseStatus, engines};
use crate::error::ServerError;
use crate::ids::{BackupId, DatabaseId, RestoreDrillId, UserId};
use crate::jobs::{self, Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::state::AppState;

pub use routes::router;

/// Memory of the copy when the database has no limit of its own.
const DEFAULT_MEMORY_MB: i64 = 1024;
const MIN_MEMORY_MB: i64 = 256;
/// The copy shares the server with the real thing: it never gets more than this.
const MAX_MEMORY_MB: i64 = 4096;
const DEFAULT_CPUS: f64 = 1.0;
const MIN_CPUS: f64 = 0.25;
const MAX_CPUS: f64 = 2.0;
/// How long the agent waits for the copy's engine to accept connections.
pub const READY_TIMEOUT_S: u32 = 300;

/// Lifecycle of one drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum DrillStatus {
	Queued,
	Running,
	Passed,
	Failed,
}

/// A row of `restore_drills`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Drill {
	pub id: RestoreDrillId,
	pub database_id: DatabaseId,
	/// `None` once retention removed the backup.
	pub backup_id: Option<BackupId>,
	/// When the backup used was taken; kept after the backup is gone.
	pub backup_taken_at: Option<OffsetDateTime>,
	pub status: DrillStatus,
	/// Tables (collections for `MongoDB`) the copy held after the restore.
	pub tables_found: Option<i64>,
	/// Tables the database held when the backup was taken, when that was counted.
	pub source_tables: Option<i64>,
	pub error: Option<String>,
	/// `None` for scheduled drills.
	pub triggered_by: Option<UserId>,
	pub created_at: OffsetDateTime,
	pub started_at: Option<OffsetDateTime>,
	pub finished_at: Option<OffsetDateTime>,
	pub duration_ms: Option<i64>,
}

/// The backup a drill would use now.
#[derive(Debug, Clone)]
pub struct Candidate {
	pub id: BackupId,
	pub taken_at: OffsetDateTime,
	pub source_tables: Option<i64>,
}

/// Whether the copy passed: it has tables, and at least as many as the database had when
/// the backup was taken. A database that was empty then may come back empty.
///
/// # Errors
/// Why the drill failed, in words for the database owner.
pub fn judge(found: u64, source: Option<i64>) -> Result<(), String> {
	let source = source.and_then(|s| u64::try_from(s).ok());
	match source {
		Some(0) if found == 0 => Ok(()),
		_ if found == 0 => Err("the restored copy has no tables".to_owned()),
		Some(source) if found < source => Err(format!(
			"the restored copy has {found} tables, but the database had {source} when the backup was taken"
		)),
		_ => Ok(()),
	}
}

/// Memory (bytes) and CPU (cores) of the copy: the database's own limits, else a default,
/// kept within bounds so a drill can neither starve nor swamp the server.
#[must_use]
pub fn limits(memory_limit_mb: Option<i32>, cpu_limit: Option<f32>) -> (i64, f64) {
	let memory_mb = memory_limit_mb
		.map_or(DEFAULT_MEMORY_MB, i64::from)
		.clamp(MIN_MEMORY_MB, MAX_MEMORY_MB);
	let cpus = cpu_limit
		.map(f64::from)
		.filter(|c| c.is_finite())
		.unwrap_or(DEFAULT_CPUS)
		.clamp(MIN_CPUS, MAX_CPUS);
	(memory_mb << 20, cpus)
}

/// Why a drill of this database cannot be started, if it cannot.
///
/// # Errors
/// [`ServerError::Validation`] for an engine without backups or a database being deleted.
pub fn drillable(database: &Database) -> Result<(), ServerError> {
	if database.engine() == Engine::Redis {
		return Err(ServerError::Validation(
			"redis databases have no backups to drill".to_owned(),
		));
	}
	if database.status == DatabaseStatus::Deleting {
		return Err(ServerError::Validation(
			"the database is being deleted".to_owned(),
		));
	}
	Ok(())
}

// --- store ------------------------------------------------------------------------------

/// The newest succeeded backup whose dump is still reachable.
pub async fn candidate(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
) -> Result<Option<Candidate>, sqlx::Error> {
	sqlx::query_as!(
		Candidate,
		r#"
			SELECT id AS "id!: BackupId", COALESCE(finished_at, created_at) AS "taken_at!",
				source_tables
			FROM backups
			WHERE database_id = $1 AND status = 'succeeded'
				AND object_key IS NOT NULL AND target_id IS NOT NULL
			ORDER BY id DESC
			LIMIT 1
		"#,
		database as DatabaseId,
	)
	.fetch_optional(db)
	.await
}

async fn insert(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
	backup: Option<&Candidate>,
	status: DrillStatus,
	error: Option<&str>,
	triggered_by: Option<UserId>,
) -> Result<Drill, sqlx::Error> {
	sqlx::query_as!(
		Drill,
		r#"
			INSERT INTO restore_drills
				(id, database_id, backup_id, backup_taken_at, source_tables, status, error,
				 triggered_by, finished_at)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
			RETURNING
				id AS "id!: RestoreDrillId", database_id AS "database_id!: DatabaseId",
				backup_id AS "backup_id: BackupId", backup_taken_at,
				status AS "status!: DrillStatus", tables_found, source_tables, error,
				triggered_by AS "triggered_by: UserId", created_at AS "created_at!",
				started_at, finished_at, duration_ms
		"#,
		RestoreDrillId::generate() as RestoreDrillId,
		database as DatabaseId,
		backup.map(|b| b.id) as Option<BackupId>,
		backup.map(|b| b.taken_at),
		backup.and_then(|b| b.source_tables),
		status as DrillStatus,
		error,
		triggered_by as Option<UserId>,
		matches!(status, DrillStatus::Passed | DrillStatus::Failed).then(OffsetDateTime::now_utc),
	)
	.fetch_one(db)
	.await
}

/// Looks up a drill by id alone, with no team check. Only for jobs.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: RestoreDrillId,
) -> Result<Option<Drill>, sqlx::Error> {
	sqlx::query_as!(
		Drill,
		r#"
			SELECT
				id AS "id!: RestoreDrillId", database_id AS "database_id!: DatabaseId",
				backup_id AS "backup_id: BackupId", backup_taken_at,
				status AS "status!: DrillStatus", tables_found, source_tables, error,
				triggered_by AS "triggered_by: UserId", created_at, started_at, finished_at,
				duration_ms
			FROM restore_drills WHERE id = $1
		"#,
		id as RestoreDrillId,
	)
	.fetch_optional(db)
	.await
}

/// A database's drills, newest first. `before` is the keyset cursor.
pub async fn list(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
	before: Option<RestoreDrillId>,
	limit: i64,
) -> Result<Vec<Drill>, sqlx::Error> {
	sqlx::query_as!(
		Drill,
		r#"
			SELECT
				id AS "id!: RestoreDrillId", database_id AS "database_id!: DatabaseId",
				backup_id AS "backup_id: BackupId", backup_taken_at,
				status AS "status!: DrillStatus", tables_found, source_tables, error,
				triggered_by AS "triggered_by: UserId", created_at, started_at, finished_at,
				duration_ms
			FROM restore_drills
			WHERE database_id = $1 AND ($2::BIGINT IS NULL OR id < $2)
			ORDER BY id DESC
			LIMIT $3
		"#,
		database as DatabaseId,
		before.map(|b| b.0),
		limit,
	)
	.fetch_all(db)
	.await
}

async fn active_exists(db: impl PgExecutor<'_>, database: DatabaseId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar!(
		r#"SELECT EXISTS (SELECT 1 FROM restore_drills
			WHERE database_id = $1 AND status IN ('queued', 'running')) AS "exists!""#,
		database as DatabaseId,
	)
	.fetch_one(db)
	.await
}

async fn mark_running(db: impl PgExecutor<'_>, id: RestoreDrillId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE restore_drills SET status = 'running', started_at = NOW() WHERE id = $1",
		id as RestoreDrillId,
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn record_found(
	db: impl PgExecutor<'_>,
	id: RestoreDrillId,
	tables: i64,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE restore_drills SET tables_found = $2 WHERE id = $1",
		id as RestoreDrillId,
		tables,
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn mark_passed(db: impl PgExecutor<'_>, id: RestoreDrillId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE restore_drills SET status = 'passed', finished_at = NOW(),
			duration_ms = (EXTRACT(EPOCH FROM (NOW() - started_at)) * 1000)::BIGINT
		WHERE id = $1 AND status = 'running'",
		id as RestoreDrillId,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Reflects a failed or cancelled drill job on its row. The status guard keeps a finished
/// drill from being rewritten.
///
/// # Errors
/// A database error.
pub async fn fail(
	state: &AppState,
	id: i64,
	failure: &JobFailure,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
	let message = match failure {
		JobFailure::Cancelled => "cancelled",
		JobFailure::Failed(m) => m.as_str(),
	};
	sqlx::query(
		"UPDATE restore_drills SET status = 'failed', error = $2, finished_at = NOW(),
			duration_ms = (EXTRACT(EPOCH FROM (NOW() - started_at)) * 1000)::BIGINT
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(message)
	.execute(&state.db)
	.await
}

// --- queueing ---------------------------------------------------------------------------

/// Queues a drill of `database` with its newest succeeded backup. The single entry point
/// for the manual route and the scheduler.
///
/// # Errors
/// [`ServerError::Validation`] when the engine has no backups or there is no backup to use
/// yet, [`ServerError::Conflict`] when a drill of the database is already queued or
/// running, or a database error. A drill whose job could not be enqueued is marked
/// `failed` first.
pub async fn queue(
	state: &AppState,
	database: &Database,
	triggered_by: Option<UserId>,
) -> Result<Drill, ServerError> {
	drillable(database)?;
	if active_exists(&state.db, database.id).await? {
		return Err(ServerError::Conflict(
			"a restore drill of this database is already queued or running".to_owned(),
		));
	}
	let backup = candidate(&state.db, database.id)
		.await?
		.ok_or_else(|| ServerError::Validation(NO_BACKUP.to_owned()))?;
	let drill = insert(
		&state.db,
		database.id,
		Some(&backup),
		DrillStatus::Queued,
		None,
		triggered_by,
	)
	.await?;
	if let Err(e) = jobs::enqueue(
		state,
		JobKind::RestoreDrill,
		database.server_id,
		drill.id.0,
		format!("drill:{}", database.id),
	)
	.await
	{
		let failure = JobFailure::Failed("failed to queue the restore drill".to_owned());
		if let Err(mark) = fail(state, drill.id.0, &failure).await {
			warn!(
				drill = drill.id.0,
				"failed to mark unqueued drill failed: {mark}"
			);
		}
		return Err(e.into());
	}
	Ok(drill)
}

/// What a scheduled drill says when there is nothing to restore.
pub const NO_BACKUP: &str = "there is no succeeded backup to drill yet";

/// Records a scheduled drill that could not start because there is no backup to restore,
/// and raises the notification: a schedule that finds no backup is exactly what a drill
/// is meant to catch.
async fn record_without_backup(state: &AppState, database: &Database) {
	match insert(
		&state.db,
		database.id,
		None,
		DrillStatus::Failed,
		Some(NO_BACKUP),
		None,
	)
	.await
	{
		Ok(_) => {
			crate::notifications::hooks::restore_drill_failed(state, database.id.0, NO_BACKUP)
				.await;
		}
		Err(e) => warn!(
			database = database.id.0,
			"cannot record a drill without a backup: {e}"
		),
	}
}

// --- the job ----------------------------------------------------------------------------

/// What the agent is asked to do: the database's image and environment in a copy with
/// limits, fed from a presigned GET of the dump.
#[must_use]
pub fn request(
	drill: RestoreDrillId,
	database: &Database,
	credentials: &databases::Credentials,
	url: String,
) -> DrillRequest {
	let engine = engines::spec(database.engine());
	let (memory_bytes, cpu_cores) = limits(database.memory_limit_mb, database.cpu_limit);
	DrillRequest {
		drill_id: drill.0,
		engine: database.engine(),
		image: engine.image(&database.version),
		env: engine.env(credentials),
		command: engine
			.command
			.map(|c| c.iter().map(|part| (*part).to_owned()).collect()),
		memory_bytes,
		cpu_cores,
		ready_timeout_s: READY_TIMEOUT_S,
		url,
	}
}

/// Runs one `RestoreDrill` job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`] with why
/// the drill failed; the worker records it on the drill row and raises the notification.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let id = RestoreDrillId(job.target_id);
	let drill = find_by_id(&state.db, id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("restore drill not found"))?;
	mark_running(&state.db, id)
		.await
		.map_err(|e| db_failure(&e))?;
	let backup = drill
		.backup_id
		.ok_or_else(|| failed("the backup was removed before the drill ran"))?;
	let r = super::resolve(state, backup).await?;
	if r.backup.status != BackupStatus::Succeeded {
		return Err(failed("only a succeeded backup can be drilled"));
	}
	let key = r
		.backup
		.object_key
		.as_deref()
		.ok_or_else(|| failed("backup has no stored dump"))?;
	let credentials = databases::decrypt_credentials(&state.secrets, &r.database).map_err(|e| {
		tracing::error!(database = r.database.id.0, "{e}");
		failed("failed to read database credentials")
	})?;
	let url = s3::presign(&r.target, &r.secret, s3::Method::Get, key)
		.map_err(|e| failed(e.to_string()))?;
	let body = request(id, &r.database, &credentials, url.to_string());

	log.push(
		LogStream::System,
		&format!(
			"drilling backup {} of {} in a throwaway copy ({} MiB, {} CPU)",
			r.backup.id,
			r.database.name,
			body.memory_bytes >> 20,
			body.cpu_cores
		),
	)
	.await;
	let Event::TablesCounted { tables } =
		call(&r.client, "/v1/backups/drill", &body, cancel, log).await?
	else {
		return Err(failed("server agent finished the drill without a result"));
	};
	let found = i64::try_from(tables).map_err(|_| failed("table count out of range"))?;
	record_found(&state.db, id, found)
		.await
		.map_err(|e| db_failure(&e))?;
	judge(tables, drill.source_tables).map_err(failed)?;
	mark_passed(&state.db, id)
		.await
		.map_err(|e| db_failure(&e))?;
	let compared = drill
		.source_tables
		.map_or_else(String::new, |s| format!(" (the database had {s})"));
	log.push(
		LogStream::System,
		&format!("drill passed: {tables} tables restored{compared}"),
	)
	.await;
	Ok(())
}
