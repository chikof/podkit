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
	};
	if let Err(e) = result {
		tracing::warn!(
			?kind,
			target_id,
			"failed to record job failure on its target: {e}"
		);
	}
}
