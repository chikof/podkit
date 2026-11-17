//! Durable job queue: rows in `jobs` are claimed by [`worker`] and executed
//! against a server's agent. This module holds the row type, enqueueing and
//! the mapping from a failed job onto the row it was acting on.

pub mod worker;

use sqlx::PgExecutor;

use crate::ids::{JobId, ServerId};
use crate::state::AppState;

/// What a job does; `target_id` points at the row of the matching table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
	/// `target_id` is a deployment id.
	Deploy,
	/// `target_id` is a server id.
	ServerBootstrap,
	/// `target_id` is a database id.
	DatabaseProvision,
	/// `target_id` is a database id.
	DatabaseStart,
	/// `target_id` is a database id.
	DatabaseStop,
	/// `target_id` is a database id.
	DatabaseRemove,
	/// `target_id` is a database id.
	DatabasePurge,
	/// `target_id` is a backup id.
	Backup,
	/// `target_id` is a backup id, not a database id.
	Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
	Queued,
	Running,
	Succeeded,
	Failed,
	Cancelled,
}

/// A row of `jobs`.
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(
	dead_code,
	reason = "columns are read by the slices that run and display jobs"
)]
pub struct Job {
	pub id: JobId,
	pub kind: JobKind,
	pub server_id: ServerId,
	pub target_id: i64,
	pub lock_key: String,
	pub status: JobStatus,
	pub error: Option<String>,
}

/// Why a job did not succeed.
#[derive(Debug, Clone)]
pub enum JobFailure {
	Cancelled,
	Failed(String),
}

/// Inserts a queued job and wakes the dispatcher. Jobs sharing a `lock_key`
/// never run concurrently.
pub async fn enqueue(
	state: &AppState,
	kind: JobKind,
	server: ServerId,
	target_id: i64,
	lock_key: String,
) -> Result<JobId, sqlx::Error> {
	let id = JobId::generate();
	sqlx::query(
		"INSERT INTO jobs (id, kind, server_id, target_id, lock_key) VALUES ($1, $2, $3, $4, $5)",
	)
	.bind(id)
	.bind(kind)
	.bind(server)
	.bind(target_id)
	.bind(lock_key)
	.execute(&state.db)
	.await?;
	state.jobs.notify.notify_one();
	Ok(id)
}

/// The newest job of `kind` for `target_id`.
pub async fn latest_for(
	db: impl PgExecutor<'_>,
	kind: JobKind,
	target_id: i64,
) -> Result<Option<Job>, sqlx::Error> {
	sqlx::query_as::<_, Job>(
		"SELECT id, kind, server_id, target_id, lock_key, status, error FROM jobs
		WHERE kind = $1 AND target_id = $2 ORDER BY id DESC LIMIT 1",
	)
	.bind(kind)
	.bind(target_id)
	.fetch_optional(db)
	.await
}

/// The newest provision, start, stop, remove or purge job of a database.
pub async fn latest_for_database(
	db: impl PgExecutor<'_>,
	database: i64,
) -> Result<Option<Job>, sqlx::Error> {
	sqlx::query_as::<_, Job>(
		"SELECT id, kind, server_id, target_id, lock_key, status, error FROM jobs
		WHERE kind IN ('database_provision', 'database_start', 'database_stop',
			'database_remove', 'database_purge') AND target_id = $1
		ORDER BY id DESC LIMIT 1",
	)
	.bind(database)
	.fetch_optional(db)
	.await
}

/// Whether a queued or running job of `kind` exists for `target_id`.
pub async fn active_exists(
	db: impl PgExecutor<'_>,
	kind: JobKind,
	target_id: i64,
) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM jobs
		WHERE kind = $1 AND target_id = $2 AND status IN ('queued', 'running'))",
	)
	.bind(kind)
	.bind(target_id)
	.fetch_one(db)
	.await
}

/// Reflects a failed or cancelled job onto the row it was acting on. A
/// missing row is ignored; database errors are logged, not returned, because
/// callers are already on a failure path.
pub async fn fail_target(state: &AppState, kind: JobKind, target_id: i64, failure: &JobFailure) {
	let result = match kind {
		JobKind::Deploy => {
			let (status, message) = match failure {
				JobFailure::Cancelled => ("cancelled", "cancelled"),
				JobFailure::Failed(m) => ("failed", m.as_str()),
			};
			// The status guard keeps a deployment that already went live from being rewritten.
			sqlx::query(
				"UPDATE deployments SET status = $2, error_message = $3, finished_at = NOW()
				WHERE id = $1 AND status IN ('queued', 'building', 'deploying')",
			)
			.bind(target_id)
			.bind(status)
			.bind(message)
			.execute(&state.db)
			.await
		}
		JobKind::ServerBootstrap => {
			let message = match failure {
				JobFailure::Cancelled => "agent bootstrap cancelled",
				JobFailure::Failed(m) => m.as_str(),
			};
			sqlx::query(
				"UPDATE servers SET status = 'unreachable', status_message = $2, updated_at = NOW()
				WHERE id = $1",
			)
			.bind(target_id)
			.bind(message)
			.execute(&state.db)
			.await
		}
		JobKind::DatabaseProvision | JobKind::DatabaseStart | JobKind::DatabaseStop => {
			let message = match failure {
				JobFailure::Cancelled => "cancelled",
				JobFailure::Failed(m) => m.as_str(),
			};
			sqlx::query(
				"UPDATE databases SET status = 'failed', status_message = $2, updated_at = NOW()
				WHERE id = $1",
			)
			.bind(target_id)
			.bind(message)
			.execute(&state.db)
			.await
		}
		JobKind::DatabaseRemove | JobKind::DatabasePurge => {
			sqlx::query("UPDATE databases SET status = 'failed', updated_at = NOW() WHERE id = $1")
				.bind(target_id)
				.execute(&state.db)
				.await
		}
		JobKind::Backup => {
			let message = match failure {
				JobFailure::Cancelled => "cancelled",
				JobFailure::Failed(m) => m.as_str(),
			};
			// The status guard keeps a finished backup from being rewritten.
			sqlx::query(
				"UPDATE backups SET status = 'failed', error = $2, finished_at = NOW()
				WHERE id = $1 AND status IN ('queued', 'running')",
			)
			.bind(target_id)
			.bind(message)
			.execute(&state.db)
			.await
		}
		// A failed restore leaves the dump intact and restorable, so it is
		// reported on the database instead of the backup.
		JobKind::Restore => {
			let message = match failure {
				JobFailure::Cancelled => "restore cancelled".to_owned(),
				JobFailure::Failed(m) => format!("restore failed: {m}"),
			};
			sqlx::query(
				"UPDATE databases SET status_message = $2, updated_at = NOW()
				WHERE id = (SELECT database_id FROM backups WHERE id = $1)",
			)
			.bind(target_id)
			.bind(message)
			.execute(&state.db)
			.await
		}
	};
	if let Err(e) = result {
		tracing::warn!(
			?kind,
			target_id,
			"failed to record job failure on its target: {e}"
		);
	}
}

#[cfg(test)]
mod tests {
	use sqlx::PgPool;

	use super::*;
	use crate::tests::state;

	/// Inserts a database with a succeeded backup; returns (database id, backup id).
	async fn seed_backup(db: &PgPool) -> (i64, i64) {
		let (team, user, project, server, database, backup): (i64, i64, i64, i64, i64, i64) =
			(1, 2, 3, 4, 5, 6);
		for (sql, binds) in [
			("INSERT INTO teams (id, name, slug, logo) VALUES ($1, 't', 't', '')", vec![team]),
			(
				"INSERT INTO users (id, name, email, password_hash) VALUES ($1, 'U', 'u@test.dev', 'x')",
				vec![user],
			),
			(
				"INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, 'p', 'p')",
				vec![project, team],
			),
			(
				"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local)
				VALUES ($1, $2, 'local', 'localhost', '/run/podman.sock', TRUE)",
				vec![server, team],
			),
			(
				"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials)
				VALUES ($1, $2, $3, 'db', 'postgres', '16', '\\x00')",
				vec![database, project, server],
			),
			(
				"INSERT INTO backups (id, database_id, status, triggered_by)
				VALUES ($1, $2, 'succeeded', $3)",
				vec![backup, database, user],
			),
		] {
			let mut query = sqlx::query(sql);
			for bind in binds {
				query = query.bind(bind);
			}
			query.execute(db).await.unwrap();
		}
		(database, backup)
	}

	#[sqlx::test]
	async fn failed_backup_job_marks_backup_failed(db: PgPool) {
		let (_, backup) = seed_backup(&db).await;
		sqlx::query("UPDATE backups SET status = 'running' WHERE id = $1")
			.bind(backup)
			.execute(&db)
			.await
			.unwrap();

		fail_target(
			&state(db.clone()),
			JobKind::Backup,
			backup,
			&JobFailure::Failed("dump exited with 1".into()),
		)
		.await;

		let (status, error, finished): (String, Option<String>, bool) = sqlx::query_as(
			"SELECT status, error, finished_at IS NOT NULL FROM backups WHERE id = $1",
		)
		.bind(backup)
		.fetch_one(&db)
		.await
		.unwrap();
		assert_eq!(status, "failed");
		assert_eq!(error.as_deref(), Some("dump exited with 1"));
		assert!(finished);
	}

	#[sqlx::test]
	async fn failed_restore_job_keeps_backup_and_flags_database(db: PgPool) {
		let (database, backup) = seed_backup(&db).await;

		fail_target(
			&state(db.clone()),
			JobKind::Restore,
			backup,
			&JobFailure::Failed("psql exited with 2".into()),
		)
		.await;

		let status: String = sqlx::query_scalar("SELECT status FROM backups WHERE id = $1")
			.bind(backup)
			.fetch_one(&db)
			.await
			.unwrap();
		assert_eq!(status, "succeeded");
		let (db_status, message): (String, Option<String>) =
			sqlx::query_as("SELECT status, status_message FROM databases WHERE id = $1")
				.bind(database)
				.fetch_one(&db)
				.await
				.unwrap();
		assert_eq!(db_status, "pending");
		assert_eq!(
			message.as_deref(),
			Some("restore failed: psql exited with 2")
		);
	}
}
