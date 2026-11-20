//! The Cron run job: runs one scheduled command through the server's agent, keeps its
//! output in the job log (at most 1 MiB of it), and afterwards decides whether to retry
//! and whom to tell.

use std::time::Duration;

use axum::http::Method;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use proto::{CronRunRequest, CronTarget, Event, Stream};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::{CronRun, CronTask, Kind, RunStatus, Trigger, store};
use crate::applications::{self, Application, SourceKind};
use crate::error::ServerError;
use crate::ids::{CronRunId, TeamId};
use crate::jobs::{Job, JobFailure};
use crate::logs::JobLog;
use crate::networks::attach::{Home, for_resource};
use crate::networks::{self, ResourceKind};
use crate::notifications::event::{Event as Notice, EventKind, clip};
use crate::servers::Server;
use crate::state::AppState;
use crate::{deployments, registries, scope, volumes};

/// What a run keeps of its output.
const MAX_LOG_BYTES: usize = 1 << 20;
/// The longest a retry waits, whatever the backoff has grown to.
const MAX_RETRY_DELAY: Duration = Duration::from_hours(6);
const LOST_CONNECTION: &str = "lost connection to server agent";

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("cron job database error: {e}");
	failed("database error")
}

/// How the agent's stream ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
	/// The command ended with this exit code.
	Exited(i64),
	/// It could not be run, or the connection failed.
	Failed(String),
	/// It ran past its timeout and was stopped.
	TimedOut,
	/// Someone cancelled it.
	Cancelled,
}

/// Counts what has been written and stops at [`MAX_LOG_BYTES`].
struct Capped {
	bytes: usize,
	full: bool,
}

impl Capped {
	async fn push(&mut self, log: &mut JobLog, stream: Stream, line: &str) {
		if self.full {
			return;
		}
		self.bytes += line.len() + 1;
		if self.bytes > MAX_LOG_BYTES {
			self.full = true;
			log.push(Stream::System, "log truncated at 1 MiB").await;
			return;
		}
		log.push(stream, line).await;
	}
}

/// Follows the agent's events into the log until the command ends, `timeout` passes or
/// `cancel` fires. Returning drops `events`, which closes the connection, and that is what
/// makes the agent kill the command or remove the container.
pub async fn drive(
	mut events: BoxStream<'static, Result<Event, ServerError>>,
	log: &mut JobLog,
	cancel: &CancellationToken,
	timeout: Duration,
) -> Outcome {
	let deadline = tokio::time::sleep(timeout);
	tokio::pin!(deadline);
	let mut cap = Capped {
		bytes: 0,
		full: false,
	};
	loop {
		tokio::select! {
			biased;
			() = cancel.cancelled() => return Outcome::Cancelled,
			() = &mut deadline => {
				log.push(
					Stream::System,
					&format!("timed out after {}; the command was stopped", super::service::duration(timeout.as_secs())),
				)
				.await;
				return Outcome::TimedOut;
			}
			event = events.next() => match event {
				Some(Ok(Event::Log { stream, line })) => cap.push(log, stream, &line).await,
				Some(Ok(Event::Exited { code })) => return Outcome::Exited(code),
				Some(Ok(Event::Failed { message })) => return Outcome::Failed(message),
				Some(Ok(_)) => {}
				Some(Err(e)) => {
					tracing::warn!("cron run stream failed: {e}");
					return Outcome::Failed(LOST_CONNECTION.to_owned());
				}
				None => return Outcome::Failed(LOST_CONNECTION.to_owned()),
			},
		}
	}
}

/// Runs one Cron run job. Every `Err` is put on the run row by `jobs::fail_target`.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`] with a
/// message for the person who owns the task.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let run = store::run_by_id(&state.db, CronRunId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("run not found"))?;
	let task = store::task_by_id(&state.db, run.task_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the task was deleted"))?;
	store::mark_started(&state.db, run.id)
		.await
		.map_err(|e| db_failure(&e))?;

	let request = build_request(state, &task, run.id).await?;
	let server = load_server(state, &task).await?;
	let client = state
		.agents
		.client(state, &server)
		.await
		.map_err(|e| failed(e.to_string()))?;
	let events = client
		.stream(Method::POST, "/v1/cron/run", Some(&request))
		.await
		.map_err(|e| failed(e.to_string()))?;

	let timeout = Duration::from_secs(u64::try_from(task.timeout_s).unwrap_or(300));
	match drive(events, log, cancel, timeout).await {
		Outcome::Exited(0) => {
			store::mark_finished(&state.db, run.id, RunStatus::Succeeded, Some(0), None)
				.await
				.map_err(|e| db_failure(&e))?;
			Ok(())
		}
		Outcome::Exited(code) => {
			let code = i32::try_from(code).unwrap_or(i32::MAX);
			store::mark_finished(
				&state.db,
				run.id,
				RunStatus::Failed,
				Some(code),
				Some(&format!("exited with code {code}")),
			)
			.await
			.map_err(|e| db_failure(&e))?;
			Err(failed(format!("exited with code {code}")))
		}
		Outcome::Failed(message) => Err(failed(message)),
		Outcome::TimedOut => Err(failed(format!(
			"timed out after {}",
			super::service::duration(timeout.as_secs())
		))),
		Outcome::Cancelled => Err(JobFailure::Cancelled),
	}
}

/// The job has no team context, so the server is resolved through the task's project.
async fn load_server(state: &AppState, task: &CronTask) -> Result<Server, JobFailure> {
	let team = team_of(state, task).await?;
	scope::server(&state.db, team, task.server_id)
		.await
		.map_err(|e| match e {
			ServerError::ServerNotFound => failed("the task's server no longer exists"),
			ServerError::Sqlx(e) => db_failure(&e),
			_ => failed("failed to look up the task's server"),
		})
}

async fn team_of(state: &AppState, task: &CronTask) -> Result<TeamId, JobFailure> {
	sqlx::query_scalar::<_, TeamId>("SELECT team_id FROM projects WHERE id = $1")
		.bind(task.project_id)
		.fetch_optional(&state.db)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("project not found"))
}

async fn application_of(state: &AppState, task: &CronTask) -> Result<Application, JobFailure> {
	let id = task
		.application_id
		.ok_or_else(|| failed("the task has no application"))?;
	applications::find_by_id(&state.db, id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the application no longer exists"))
}

/// What the agent is asked to run.
///
/// # Errors
/// A [`JobFailure`] saying what the task needs and does not have: a running application,
/// an image, a decryptable variable.
pub async fn build_request(
	state: &AppState,
	task: &CronTask,
	run: CronRunId,
) -> Result<CronRunRequest, JobFailure> {
	let target = match task.kind {
		Kind::Exec => {
			let app = application_of(state, task).await?;
			let container = deployments::latest_running(&state.db, app.id)
				.await
				.map_err(|e| db_failure(&e))?
				.and_then(|d| d.container_id)
				.ok_or_else(|| failed("the application is not running"))?;
			CronTarget::Exec {
				container,
				command: task.command.clone(),
			}
		}
		Kind::Container => container_target(state, task).await?,
	};
	Ok(CronRunRequest {
		run_id: run.0,
		target,
	})
}

async fn container_target(state: &AppState, task: &CronTask) -> Result<CronTarget, JobFailure> {
	let Some(_) = task.application_id else {
		let image = task
			.image
			.clone()
			.ok_or_else(|| failed("the task has no image"))?;
		let network = networks::ensure_environment(&state.db, task.environment_id, task.server_id)
			.await
			.map_err(|e| db_failure(&e))?;
		return Ok(CronTarget::Container {
			image,
			auth: None,
			command: task.command.clone(),
			env: Vec::new(),
			attach: vec![proto::NetworkAttach {
				name: network.podman_name,
				aliases: Vec::new(),
				internal: network.internal,
			}],
			memory_bytes: None,
			cpu_cores: None,
			volumes: Vec::new(),
		});
	};
	let app = application_of(state, task).await?;
	let image = match &task.image {
		Some(image) => image.clone(),
		None => app_image(state, &app).await?,
	};
	let auth = match app.registry_id {
		Some(id) => Some(
			registries::service::auth_for(state, id)
				.await
				.map_err(|e| {
					tracing::error!(application = app.id.0, "saved registry unusable: {e}");
					failed("failed to load the saved registry login")
				})?,
		),
		None => deployments::run::registry_auth(state, &app)?,
	};
	let attach = for_resource(
		&state.db,
		Home {
			kind: ResourceKind::Application,
			id: app.id.0,
			environment: app.environment_id,
			server: app.server_id,
		},
		&[],
	)
	.await
	.map_err(|e| {
		tracing::error!("failed to resolve networks: {e}");
		failed("failed to resolve the application's networks")
	})?;
	let volumes = volumes::list(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?
		.into_iter()
		.map(|v| proto::VolumeMount {
			name: v.name,
			path: v.mount_path,
			read_only: false,
		})
		.collect();
	Ok(CronTarget::Container {
		image,
		auth,
		command: task.command.clone(),
		env: deployments::run::load_env(state, &app).await?,
		attach,
		memory_bytes: app.memory_limit_mb.map(|mb| i64::from(mb) * 1024 * 1024),
		cpu_cores: app.cpu_limit.map(f64::from),
		volumes,
	})
}

/// The image the application runs now: what its newest deployment built or pulled.
async fn app_image(state: &AppState, app: &Application) -> Result<String, JobFailure> {
	if let Some(deployment) = deployments::latest_with_image(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?
		&& let Some(tag) = deployment.image_tag
	{
		return Ok(tag);
	}
	match (app.source_kind, &app.image) {
		(SourceKind::Image, Some(image)) => Ok(image.clone()),
		_ => Err(failed("the application has not been deployed yet")),
	}
}

// ----- after the run --------------------------------------------------------------------

/// Called once a run's job has ended and its row is settled. A failure that has retries
/// left is queued again, with a longer wait each time; one that has none is a `cron.failed`
/// event. A task that was failing and succeeds says so once.
pub async fn finished(state: &AppState, run: CronRunId, result: &Result<(), JobFailure>) {
	if let Err(e) = settle(state, run, result).await {
		tracing::warn!(run = run.0, "cannot settle a finished cron run: {e}");
	}
}

async fn settle(
	state: &AppState,
	run_id: CronRunId,
	result: &Result<(), JobFailure>,
) -> Result<(), sqlx::Error> {
	let Some(run) = store::run_by_id(&state.db, run_id).await? else {
		return Ok(());
	};
	let Some(task) = store::task_by_id(&state.db, run.task_id).await? else {
		return Ok(());
	};
	match result {
		Ok(()) => {
			if task.last_outcome.as_deref() == Some("failed") {
				emit(state, &task, EventKind::CronRecovered, &run, None).await;
			}
			set_outcome(state, &task, "succeeded").await
		}
		Err(JobFailure::Cancelled) => Ok(()),
		Err(JobFailure::Failed(message)) => {
			if run.attempt <= task.max_retries && task.enabled {
				return queue_retry(state, &task, &run).await;
			}
			emit(state, &task, EventKind::CronFailed, &run, Some(message)).await;
			set_outcome(state, &task, "failed").await
		}
	}
}

async fn set_outcome(state: &AppState, task: &CronTask, outcome: &str) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE cron_tasks SET last_outcome = $2 WHERE id = $1")
		.bind(task.id)
		.bind(outcome)
		.execute(&state.db)
		.await
		.map(drop)
}

/// The wait before attempt `attempt + 1`: the task's backoff, doubled for each attempt
/// already made.
#[must_use]
pub fn retry_delay(backoff_s: i32, attempt: i32) -> Duration {
	let base = Duration::from_secs(u64::try_from(backoff_s).unwrap_or(60));
	let doublings = u32::try_from(attempt.saturating_sub(1))
		.unwrap_or(0)
		.min(16);
	base.saturating_mul(1u32 << doublings).min(MAX_RETRY_DELAY)
}

async fn queue_retry(state: &AppState, task: &CronTask, run: &CronRun) -> Result<(), sqlx::Error> {
	let now = OffsetDateTime::now_utc();
	let mut conn = state.db.acquire().await?;
	let delay = retry_delay(task.retry_backoff_s, run.attempt);
	store::enqueue_run(
		&mut conn,
		task,
		Trigger::Retry,
		run.attempt + 1,
		run.scheduled_for,
		now + delay,
	)
	.await?;
	// The dispatcher sleeps up to five seconds; wake it when the retry is due.
	let jobs = std::sync::Arc::clone(&state.jobs);
	tokio::spawn(async move {
		tokio::time::sleep(delay).await;
		jobs.notify.notify_one();
	});
	state.jobs.notify.notify_one();
	Ok(())
}

async fn emit(
	state: &AppState,
	task: &CronTask,
	kind: EventKind,
	run: &CronRun,
	message: Option<&str>,
) {
	let team = match sqlx::query_scalar::<_, TeamId>("SELECT team_id FROM projects WHERE id = $1")
		.bind(task.project_id)
		.fetch_optional(&state.db)
		.await
	{
		Ok(Some(team)) => team,
		Ok(None) => return,
		Err(e) => {
			tracing::warn!("cannot look up the team of a cron task: {e}");
			return;
		}
	};
	let title = match kind {
		EventKind::CronRecovered => format!("{} ran well again", task.name),
		_ => match run.attempt {
			1 => format!("{} failed", task.name),
			n => format!("{} failed after {n} attempts", task.name),
		},
	};
	let mut notice = Notice::new(team, kind, format!("cron:{}", task.id), title)
		.scope(task.project_id, Some(task.environment_id))
		.link(format!(
			"/teams/{team}/projects/{}/cron/{}",
			task.project_id, task.id
		));
	if let Some(message) = message {
		notice = notice.body(clip(message, 500));
	}
	crate::notifications::emit(state, notice).await;
}
