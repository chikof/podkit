//! Database backups: per-team S3-compatible targets, the `backups` rows, and
//! the Backup and Restore jobs that move dumps between a server's agent and
//! the target bucket.

mod routes;
pub mod s3;
pub mod schedule;
pub mod service;
pub mod targets;

use axum::http::Method;
use crypto::age::{AgeError, SecretBox};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use proto::{DumpRequest, Event, RestoreRequest, Stream as LogStream, UploadRequest};
use serde::Serialize;
use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use zeroize::Zeroizing;

use crate::agents::AgentClient;
use crate::databases::{self, Database};
use crate::error::ServerError;
use crate::ids::{BackupId, BackupTargetId, DatabaseId, ProjectId, ServerId, TeamId, UserId};
use crate::jobs::{self, Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::scope;
use crate::state::AppState;

pub use routes::router;
pub use schedule::{next_run, parse_schedule};

const LOST_CONNECTION: &str = "lost connection to server agent";

/// An S3-compatible bucket a team's databases are backed up to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BackupTarget {
	pub id: BackupTargetId,
	pub team_id: TeamId,
	pub name: String,
	pub endpoint: String,
	pub region: String,
	pub bucket: String,
	pub path_style: bool,
	/// Key prefix inside the bucket; empty or any `a/b` form.
	pub prefix: String,
	pub access_key_id: String,
	/// age-encrypted secret access key.
	pub secret_access_key: Vec<u8>,
	pub created_at: OffsetDateTime,
}

/// Lifecycle state of one backup attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum BackupStatus {
	Queued,
	Running,
	Succeeded,
	Failed,
}

/// Row from the `backups` table: one dump of one database.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Backup {
	pub id: BackupId,
	pub database_id: DatabaseId,
	/// `None` once the target was deleted; the object is then unreachable.
	pub target_id: Option<BackupTargetId>,
	pub status: BackupStatus,
	pub object_key: Option<String>,
	pub size_bytes: Option<i64>,
	pub sha256: Option<String>,
	pub error: Option<String>,
	/// `None` for scheduled backups.
	pub triggered_by: Option<UserId>,
	pub created_at: OffsetDateTime,
	pub finished_at: Option<OffsetDateTime>,
}

/// `{prefix}{team}/databases/{database}/{backup}.{ext}`, where a non-empty
/// prefix gets a trailing `/`.
fn object_key(
	prefix: &str,
	team: TeamId,
	database: DatabaseId,
	backup: BackupId,
	ext: &str,
) -> String {
	let sep = if prefix.is_empty() || prefix.ends_with('/') {
		""
	} else {
		"/"
	};
	format!("{prefix}{sep}{team}/databases/{database}/{backup}.{ext}")
}

/// Inserts a `queued` backup.
pub async fn insert(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
	target: BackupTargetId,
	triggered_by: Option<UserId>,
) -> Result<Backup, sqlx::Error> {
	sqlx::query_as!(
		Backup,
		r#"
			INSERT INTO backups (id, database_id, target_id, triggered_by)
			VALUES ($1, $2, $3, $4)
			RETURNING
				id AS "id!: BackupId", database_id AS "database_id!: DatabaseId",
				target_id AS "target_id: BackupTargetId", status AS "status!: BackupStatus",
				object_key, size_bytes, sha256, error,
				triggered_by AS "triggered_by: UserId", created_at AS "created_at!", finished_at
		"#,
		BackupId::generate() as BackupId,
		database as DatabaseId,
		target as BackupTargetId,
		triggered_by as Option<UserId>,
	)
	.fetch_one(db)
	.await
}

/// Inserts a queued backup of `database` and enqueues its Backup job. The
/// single entry point for the manual route and the scheduler.
///
/// # Errors
/// A database error; a backup whose job could not be enqueued is marked
/// `failed` first.
pub async fn queue(
	state: &AppState,
	database: DatabaseId,
	server: ServerId,
	target: BackupTargetId,
	triggered_by: Option<UserId>,
) -> Result<Backup, ServerError> {
	let backup = insert(&state.db, database, target, triggered_by).await?;
	if let Err(e) = jobs::enqueue(
		state,
		JobKind::Backup,
		server,
		backup.id.0,
		format!("db:{database}"),
	)
	.await
	{
		if let Err(mark) = mark_failed(&state.db, backup.id, "failed to queue backup job").await {
			warn!(
				backup = backup.id.0,
				"failed to mark unqueued backup failed: {mark}"
			);
		}
		return Err(e.into());
	}
	Ok(backup)
}

/// Looks up a backup by id alone, with no team check. Only for jobs.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: BackupId,
) -> Result<Option<Backup>, sqlx::Error> {
	sqlx::query_as!(
		Backup,
		r#"
			SELECT
				id AS "id!: BackupId", database_id AS "database_id!: DatabaseId",
				target_id AS "target_id: BackupTargetId", status AS "status!: BackupStatus",
				object_key, size_bytes, sha256, error,
				triggered_by AS "triggered_by: UserId", created_at, finished_at
			FROM backups WHERE id = $1
		"#,
		id as BackupId,
	)
	.fetch_optional(db)
	.await
}

/// Looks up a backup scoped to its database, so a backup id of another
/// database never resolves.
pub async fn find(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
	id: BackupId,
) -> Result<Option<Backup>, sqlx::Error> {
	sqlx::query_as!(
		Backup,
		r#"
			SELECT
				id AS "id!: BackupId", database_id AS "database_id!: DatabaseId",
				target_id AS "target_id: BackupTargetId", status AS "status!: BackupStatus",
				object_key, size_bytes, sha256, error,
				triggered_by AS "triggered_by: UserId", created_at, finished_at
			FROM backups WHERE id = $2 AND database_id = $1
		"#,
		database as DatabaseId,
		id as BackupId,
	)
	.fetch_optional(db)
	.await
}

/// Lists a database's backups, newest first. `before` is the keyset cursor:
/// only ids strictly below it are returned.
pub async fn list(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
	before: Option<BackupId>,
	limit: i64,
) -> Result<Vec<Backup>, sqlx::Error> {
	sqlx::query_as!(
		Backup,
		r#"
			SELECT
				id AS "id!: BackupId", database_id AS "database_id!: DatabaseId",
				target_id AS "target_id: BackupTargetId", status AS "status!: BackupStatus",
				object_key, size_bytes, sha256, error,
				triggered_by AS "triggered_by: UserId", created_at, finished_at
			FROM backups
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

/// Succeeded backups beyond the newest `keep`, oldest last.
async fn beyond_retention(
	db: impl PgExecutor<'_>,
	database: DatabaseId,
	keep: i64,
) -> Result<Vec<Backup>, sqlx::Error> {
	sqlx::query_as!(
		Backup,
		r#"
			SELECT
				id AS "id!: BackupId", database_id AS "database_id!: DatabaseId",
				target_id AS "target_id: BackupTargetId", status AS "status!: BackupStatus",
				object_key, size_bytes, sha256, error,
				triggered_by AS "triggered_by: UserId", created_at, finished_at
			FROM backups
			WHERE database_id = $1 AND status = 'succeeded'
			ORDER BY id DESC
			OFFSET $2
		"#,
		database as DatabaseId,
		keep,
	)
	.fetch_all(db)
	.await
}

async fn mark_running(db: impl PgExecutor<'_>, id: BackupId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE backups SET status = 'running' WHERE id = $1",
		id as BackupId,
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn mark_succeeded(
	db: impl PgExecutor<'_>,
	id: BackupId,
	object_key: &str,
	size_bytes: i64,
	sha256: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE backups
		SET status = 'succeeded', object_key = $2, size_bytes = $3, sha256 = $4, finished_at = NOW()
		WHERE id = $1",
		id as BackupId,
		object_key,
		size_bytes,
		sha256,
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn mark_failed(
	db: impl PgExecutor<'_>,
	id: BackupId,
	error: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE backups SET status = 'failed', error = $2, finished_at = NOW() WHERE id = $1",
		id as BackupId,
		error,
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn delete_row(db: impl PgExecutor<'_>, id: BackupId) -> Result<(), sqlx::Error> {
	sqlx::query!("DELETE FROM backups WHERE id = $1", id as BackupId)
		.execute(db)
		.await?;
	Ok(())
}

/// Looks up a target by id alone, with no team check. Only for jobs and for
/// backups, whose target belongs to the database's team by construction.
async fn find_target(
	db: impl PgExecutor<'_>,
	id: BackupTargetId,
) -> Result<Option<BackupTarget>, sqlx::Error> {
	sqlx::query_as!(
		BackupTarget,
		r#"
			SELECT id AS "id: BackupTargetId", team_id AS "team_id: TeamId", name, endpoint,
				region, bucket, path_style, prefix, access_key_id, secret_access_key, created_at
			FROM backup_targets WHERE id = $1
		"#,
		id as BackupTargetId,
	)
	.fetch_optional(db)
	.await
}

fn decrypt_secret(
	secrets: &SecretBox,
	target: &BackupTarget,
) -> Result<Zeroizing<String>, AgeError> {
	secrets
		.decrypt(&target.secret_access_key)
		.map(Zeroizing::new)
}

/// Removes the backup's object from its target, then the row. A failed
/// backup has no object and is only deleted.
///
/// # Errors
/// [`ServerError::Validation`] with the storage error when the object cannot
/// be deleted; the row is kept so the delete can be retried.
pub async fn purge(state: &AppState, backup: &Backup) -> Result<(), ServerError> {
	if let (Some(key), Some(target_id)) = (&backup.object_key, backup.target_id)
		&& let Some(target) = find_target(&state.db, target_id).await?
	{
		let secret = decrypt_secret(&state.secrets, &target).map_err(|e| {
			tracing::error!(
				backup_target = target.id.0,
				"cannot decrypt backup target secret: {e}"
			);
			ServerError::Internal
		})?;
		s3::delete(&state.http, &target, &secret, key)
			.await
			.map_err(|e| {
				ServerError::Validation(format!("could not delete the backup from storage: {e}"))
			})?;
	}
	delete_row(&state.db, backup.id).await?;
	Ok(())
}

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("backup job database error: {e}");
	failed("database error")
}

/// Everything a Backup or Restore job needs, resolved up front so a missing
/// piece fails the job before it touches the agent.
struct Prepared {
	backup: Backup,
	database: Database,
	target: BackupTarget,
	secret: Zeroizing<String>,
	team: TeamId,
	container: String,
	client: AgentClient,
}

async fn team_of(db: &PgPool, project: ProjectId) -> Result<TeamId, JobFailure> {
	sqlx::query_scalar!(
		r#"SELECT team_id AS "team_id!: TeamId" FROM projects WHERE id = $1"#,
		project.0,
	)
	.fetch_optional(db)
	.await
	.map_err(|e| db_failure(&e))?
	.ok_or_else(|| failed("project not found"))
}

/// The job runs without a team context, so the server is resolved through
/// the database's project.
async fn prepare(state: &AppState, job: &Job) -> Result<Prepared, JobFailure> {
	let backup = find_by_id(&state.db, BackupId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("backup not found"))?;
	let database = databases::find_by_id(&state.db, backup.database_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("database not found"))?;
	let container = database
		.container_id
		.clone()
		.ok_or_else(|| failed("database has no container"))?;
	let target_id = backup
		.target_id
		.ok_or_else(|| failed("the backup target was deleted"))?;
	let target = find_target(&state.db, target_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("backup target not found"))?;
	let secret = decrypt_secret(&state.secrets, &target).map_err(|e| {
		warn!(
			backup_target = target.id.0,
			"cannot decrypt backup target secret: {e}"
		);
		failed("cannot decrypt the backup target secret")
	})?;

	let team = team_of(&state.db, database.project_id).await?;
	if target.team_id != team {
		return Err(failed("backup target belongs to another team"));
	}
	let server = scope::server(&state.db, team, database.server_id)
		.await
		.map_err(|e| match e {
			ServerError::ServerNotFound => failed("target server not found"),
			ServerError::Sqlx(e) => db_failure(&e),
			_ => failed("failed to look up target server"),
		})?;
	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;

	Ok(Prepared {
		backup,
		database,
		target,
		secret,
		team,
		container,
		client,
	})
}

/// Posts `body` to a streaming agent endpoint and returns its terminal event,
/// mirroring log lines onto the job log on the way. Dropping the stream on
/// cancel closes the connection, which stops the agent's work.
async fn call(
	client: &AgentClient,
	path: &str,
	body: &(impl Serialize + Sync),
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<Event, JobFailure> {
	let mut events: BoxStream<'static, Result<Event, ServerError>> = client
		.stream(Method::POST, path, Some(body))
		.await
		.map_err(|e| match e {
			ServerError::Validation(message) | ServerError::Conflict(message) => failed(message),
			e => {
				warn!(path, "failed to start agent stream: {e}");
				failed("server agent unavailable")
			}
		})?;

	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &line).await,
			Some(Ok(Event::Phase { .. })) => {}
			Some(Ok(Event::Failed { message })) => return Err(JobFailure::Failed(message)),
			Some(Ok(terminal)) => return Ok(terminal),
			Some(Err(e)) => {
				warn!(path, "agent stream failed: {e}");
				return Err(failed(LOST_CONNECTION));
			}
			None => return Err(failed(LOST_CONNECTION)),
		}
	}
}

/// Runs one Backup job: dump on the agent, upload through a presigned PUT,
/// record the result, then prune backups beyond the database's retention.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`]; the worker mirrors it onto the backup row.
pub async fn run_backup(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let p = prepare(state, job).await?;
	let engine = p.database.engine();
	let ext = engine
		.dump_ext()
		.ok_or_else(|| failed("redis backups are not supported"))?;

	mark_running(&state.db, p.backup.id)
		.await
		.map_err(|e| db_failure(&e))?;

	let dump = DumpRequest {
		backup_id: p.backup.id.0,
		container: p.container.clone(),
		engine,
	};
	let Event::DumpWritten { path, size, sha256 } =
		call(&p.client, "/v1/backups/dump", &dump, cancel, log).await?
	else {
		return Err(failed("server agent finished the dump without a file"));
	};
	let size_bytes = i64::try_from(size).map_err(|_| failed("dump size out of range"))?;

	let key = object_key(&p.target.prefix, p.team, p.database.id, p.backup.id, ext);
	let url = s3::presign(&p.target, &p.secret, s3::Method::Put, &key)
		.map_err(|e| failed(e.to_string()))?;
	let upload = UploadRequest {
		path,
		url: url.to_string(),
	};
	let Event::Done = call(&p.client, "/v1/backups/upload", &upload, cancel, log).await? else {
		return Err(failed(
			"server agent finished the upload without confirming it",
		));
	};

	mark_succeeded(&state.db, p.backup.id, &key, size_bytes, &sha256)
		.await
		.map_err(|e| db_failure(&e))?;
	log.push(LogStream::System, &format!("backup stored as {key}"))
		.await;

	enforce_retention(state, &p, log).await;
	Ok(())
}

/// Best effort: the new backup already succeeded, so a failure here is
/// logged and retried by the next backup.
async fn enforce_retention(state: &AppState, p: &Prepared, log: &mut JobLog) {
	let old =
		match beyond_retention(&state.db, p.database.id, i64::from(p.database.backup_keep)).await {
			Ok(old) => old,
			Err(e) => {
				warn!(
					database = p.database.id.0,
					"cannot list backups beyond retention: {e}"
				);
				return;
			}
		};
	for backup in old {
		match purge(state, &backup).await {
			Ok(()) => {
				log.push(
					LogStream::System,
					&format!("removed old backup {}", backup.id),
				)
				.await;
			}
			Err(e) => warn!(
				backup = backup.id.0,
				"cannot remove backup beyond retention: {e}"
			),
		}
	}
}

/// Runs one Restore job: the agent streams the dump from a presigned GET
/// into the database's restore command.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`]; the worker reports it on the database row.
pub async fn run_restore(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let p = prepare(state, job).await?;
	if p.backup.status != BackupStatus::Succeeded {
		return Err(failed("only a succeeded backup can be restored"));
	}
	let key = p
		.backup
		.object_key
		.as_deref()
		.ok_or_else(|| failed("backup has no stored dump"))?;

	let url = s3::presign(&p.target, &p.secret, s3::Method::Get, key)
		.map_err(|e| failed(e.to_string()))?;
	let restore = RestoreRequest {
		container: p.container.clone(),
		engine: p.database.engine(),
		url: url.to_string(),
	};
	let Event::Done = call(&p.client, "/v1/backups/restore", &restore, cancel, log).await? else {
		return Err(failed(
			"server agent finished the restore without confirming it",
		));
	};
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	const TEAM: TeamId = TeamId(1);
	const DATABASE: DatabaseId = DatabaseId(2);
	const BACKUP: BackupId = BackupId(3);

	#[test]
	fn object_key_without_prefix() {
		assert_eq!(
			object_key("", TEAM, DATABASE, BACKUP, "sql.gz"),
			"1/databases/2/3.sql.gz"
		);
	}

	#[test]
	fn object_key_adds_the_missing_slash_after_a_prefix() {
		assert_eq!(
			object_key("podkit", TEAM, DATABASE, BACKUP, "archive.gz"),
			"podkit/1/databases/2/3.archive.gz"
		);
		assert_eq!(
			object_key("a/b", TEAM, DATABASE, BACKUP, "sql.gz"),
			"a/b/1/databases/2/3.sql.gz"
		);
	}

	#[test]
	fn object_key_keeps_an_existing_trailing_slash() {
		assert_eq!(
			object_key("podkit/", TEAM, DATABASE, BACKUP, "sql.gz"),
			"podkit/1/databases/2/3.sql.gz"
		);
	}
}
