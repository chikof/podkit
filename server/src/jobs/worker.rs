//! Claims queued jobs and runs them on the task tracker.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::{Job, JobFailure, JobKind, JobStatus, fail_target};
use crate::ids::JobId;
use crate::logs::JobLog;
use crate::state::AppState;

/// Concurrent running jobs allowed per server. Scans and cron runs do not count: scans have
/// a lane of their own (one at a time per server, by lock key), and a run of a scheduled
/// task is policed by its task's concurrency policy, so a deploy never waits for either and
/// neither waits for a deploy. Nor does an environment clone: it only waits for the jobs it
/// queues, which need the slots.
const MAX_JOBS_PER_SERVER: i64 = 2;

const INTERRUPTED: &str = "interrupted: podkit restarted";

/// Dispatcher wakeups, cancellation handles of running jobs and their tasks.
#[derive(Default)]
pub struct Jobs {
	pub notify: Notify,
	running: Mutex<HashMap<JobId, CancellationToken>>,
	pub tracker: TaskTracker,
}

impl Jobs {
	pub fn new() -> Self {
		Self::default()
	}

	fn running(&self) -> MutexGuard<'_, HashMap<JobId, CancellationToken>> {
		self.running.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

/// Fails every job left `running` by a previous process. Run before the dispatcher.
pub async fn recover(state: &AppState) -> Result<(), sqlx::Error> {
	let rows = sqlx::query_as::<_, (JobKind, i64)>(
		"UPDATE jobs SET status = 'failed', error = $1, finished_at = NOW()
		WHERE status = 'running' RETURNING kind, target_id",
	)
	.bind(INTERRUPTED)
	.fetch_all(&state.db)
	.await?;
	for (kind, target_id) in rows {
		fail_target(
			state,
			kind,
			target_id,
			&JobFailure::Failed(INTERRUPTED.to_owned()),
		)
		.await;
	}
	Ok(())
}

/// Claims and spawns jobs until `shutdown` is cancelled. Shutdown only stops
/// claiming; running jobs get independent tokens that only `cancel()` fires.
pub async fn run_dispatcher(state: AppState, shutdown: CancellationToken) {
	while !shutdown.is_cancelled() {
		loop {
			match claim(&state.db, MAX_JOBS_PER_SERVER).await {
				Ok(Some(job)) => {
					let token = CancellationToken::new();
					// Registered before spawning so a cancel cannot miss a claimed job.
					state.jobs.running().insert(job.id, token.clone());
					state.jobs.tracker.spawn(run_job(state.clone(), job, token));
				}
				Ok(None) => break,
				Err(e) => {
					tracing::error!("failed to claim a job: {e}");
					break;
				}
			}
		}
		// Not a service of its own: the delivery queue is worked from this loop.
		crate::notifications::delivery::wake(&state);
		crate::cron::scheduler::tick(&state).await;
		crate::uptime::runner::wake(&state);
		tokio::select! {
			() = state.jobs.notify.notified() => {}
			() = tokio::time::sleep(Duration::from_secs(5)) => {}
			() = shutdown.cancelled() => {}
		}
	}
}

/// Atomically moves the oldest runnable job to `running`. A job is runnable
/// when no running job holds its lock key and its server has fewer than
/// `max` running jobs.
async fn claim(db: &PgPool, max: i64) -> Result<Option<Job>, sqlx::Error> {
	sqlx::query_as::<_, Job>(
		"UPDATE jobs SET status = 'running', started_at = NOW() WHERE id = (
			SELECT j.id FROM jobs j
			WHERE j.status = 'queued' AND j.run_after <= NOW()
				AND NOT EXISTS (SELECT 1 FROM jobs r WHERE r.status = 'running' AND r.lock_key = j.lock_key)
				AND (j.kind IN ('image_scan', 'image_rescan', 'cron_run', 'environment_clone')
					OR (SELECT COUNT(*) FROM jobs r WHERE r.status = 'running' AND r.server_id = j.server_id
						AND r.kind NOT IN ('image_scan', 'image_rescan', 'cron_run', 'environment_clone')) < $1)
			ORDER BY j.id FOR UPDATE SKIP LOCKED LIMIT 1)
		RETURNING id, kind, server_id, target_id, lock_key, status, error",
	)
	.bind(max)
	.fetch_optional(db)
	.await
}

async fn run_job(state: AppState, job: Job, token: CancellationToken) {
	let (result, log) = match state.logs.open(job.id).await {
		Ok(mut log) => (dispatch(&state, &job, &token, &mut log).await, Some(log)),
		Err(e) => (
			Err(JobFailure::Failed(format!("cannot open job log: {e}"))),
			None,
		),
	};

	let (status, error) = match &result {
		Ok(()) => (JobStatus::Succeeded, None),
		Err(JobFailure::Cancelled) => (JobStatus::Cancelled, None),
		Err(JobFailure::Failed(message)) => (JobStatus::Failed, Some(message.as_str())),
	};
	if let Err(e) =
		sqlx::query("UPDATE jobs SET status = $2, error = $3, finished_at = NOW() WHERE id = $1")
			.bind(job.id)
			.bind(status)
			.bind(error)
			.execute(&state.db)
			.await
	{
		tracing::error!(job = %job.id, "failed to finalize job: {e}");
	}
	if let Err(failure) = &result {
		fail_target(&state, job.kind, job.target_id, failure).await;
	}
	crate::notifications::hooks::job_finished(&state, &job, &result).await;
	crate::autodeploy::engine::deploy_finished(&state, &job, &result);

	// The log ends only after the final status is stored, so a client that sees
	// `event: end` and refetches reads a settled state.
	if let Some(log) = log {
		log.finish().await;
	}
	state.jobs.running().remove(&job.id);
	state.jobs.notify.notify_one();
}

async fn dispatch(
	state: &AppState,
	job: &Job,
	token: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	match job.kind {
		JobKind::Deploy => crate::deployments::run::run(state, job, token, log).await,
		JobKind::DatabaseProvision
		| JobKind::DatabaseStart
		| JobKind::DatabaseStop
		| JobKind::DatabaseRemove
		| JobKind::DatabasePurge => crate::databases::run::run(state, job, token, log).await,
		JobKind::Backup => crate::backups::run_backup(state, job, token, log).await,
		JobKind::Restore => crate::backups::run_restore(state, job, token, log).await,
		JobKind::ServerBootstrap => crate::servers::bootstrap::run(state, job, token, log).await,
		JobKind::ModelDownload => crate::model_files::run::run(state, job, token, log).await,
		JobKind::ModelStart | JobKind::ModelStop | JobKind::ModelRemove => {
			crate::models::run::run(state, job, token, log).await
		}
		JobKind::StackDeploy
		| JobKind::StackStop
		| JobKind::StackRemove
		| JobKind::StackPurge
		| JobKind::StackRestartService => crate::stacks::run::run(state, job, token, log).await,
		JobKind::ImageScan | JobKind::ImageRescan => {
			crate::scanning::run::run(state, job, token, log).await
		}
		JobKind::CronRun => crate::cron::run::run(state, job, token, log).await,
		JobKind::VolumeBackup => {
			crate::volume_backups::run::run_backup(state, job, token, log).await
		}
		JobKind::VolumeRestore => {
			crate::volume_backups::run::run_restore(state, job, token, log).await
		}
		JobKind::EnvironmentClone => {
			crate::environments::clone::run::run(state, job, token, log).await
		}
	}
}

/// Cancels a queued job outright or signals a running one. Returns whether
/// the job was in a cancellable state.
pub async fn cancel(state: &AppState, id: JobId) -> Result<bool, sqlx::Error> {
	let queued = sqlx::query_as::<_, (JobKind, i64)>(
		"UPDATE jobs SET status = 'cancelled', finished_at = NOW()
		WHERE id = $1 AND status = 'queued' RETURNING kind, target_id",
	)
	.bind(id)
	.fetch_optional(&state.db)
	.await?;
	if let Some((kind, target_id)) = queued {
		fail_target(state, kind, target_id, &JobFailure::Cancelled).await;
		return Ok(true);
	}
	let token = state.jobs.running().get(&id).cloned();
	Ok(token.is_some_and(|t| {
		t.cancel();
		true
	}))
}

#[cfg(test)]
mod tests {
	use super::*;

	async fn seed_server(db: &PgPool) -> i64 {
		let team = crypto::generate_id();
		let server = crypto::generate_id();
		sqlx::query("INSERT INTO teams (id, name, slug, logo) VALUES ($1, 't', $2, '')")
			.bind(team)
			.bind(format!("t{team}"))
			.execute(db)
			.await
			.unwrap();
		sqlx::query(
			"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local)
			VALUES ($1, $2, 'local', 'localhost', '/run/podman.sock', TRUE)",
		)
		.bind(server)
		.bind(team)
		.execute(db)
		.await
		.unwrap();
		server
	}

	async fn queue(db: &PgPool, server: i64, lock: &str) {
		sqlx::query(
			"INSERT INTO jobs (id, kind, server_id, target_id, lock_key) VALUES ($1, 'deploy', $2, $1, $3)",
		)
		.bind(crypto::generate_id())
		.bind(server)
		.bind(lock)
		.execute(db)
		.await
		.unwrap();
	}

	#[sqlx::test]
	async fn scanning_jobs_never_take_a_deploy_slot(db: PgPool) {
		let server = seed_server(&db).await;
		// The server is busy with two deploys.
		for lock in ["app:1", "app:2"] {
			queue(&db, server, lock).await;
			claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().unwrap();
		}
		queue(&db, server, "app:3").await;
		assert!(claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().is_none());

		// A scan still runs; a second scan of the same server waits for the first.
		for kind in ["image_scan", "image_scan", "image_rescan"] {
			sqlx::query(
				"INSERT INTO jobs (id, kind, server_id, target_id, lock_key) VALUES ($1, $2, $3, $1, $4)",
			)
			.bind(crypto::generate_id())
			.bind(kind)
			.bind(server)
			.bind(format!("scan:{server}"))
			.execute(&db)
			.await
			.unwrap();
		}
		let scan = claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().unwrap();
		assert_eq!(scan.kind, super::super::JobKind::ImageScan);
		assert!(claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().is_none());
	}

	#[sqlx::test]
	async fn same_lock_key_runs_one_at_a_time(db: PgPool) {
		let server = seed_server(&db).await;
		queue(&db, server, "app:1").await;
		queue(&db, server, "app:1").await;

		let first = claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().unwrap();
		assert_eq!(first.status, JobStatus::Running);
		assert!(claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().is_none());
	}

	#[sqlx::test]
	async fn server_runs_at_most_max_jobs(db: PgPool) {
		let server = seed_server(&db).await;
		for lock in ["app:1", "app:2", "app:3"] {
			queue(&db, server, lock).await;
		}

		assert!(claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().is_some());
		assert!(claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().is_some());
		assert!(claim(&db, MAX_JOBS_PER_SERVER).await.unwrap().is_none());
	}
}
