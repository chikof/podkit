//! The preview job: brings a pull request's preview up (or redeploys it after a push), or takes
//! it down.
//!
//! Bringing up: start the fresh databases, deploy the application from the pull request's head,
//! run the post-deploy command in the container, then say so in the pull request. Taking down:
//! remove the containers and volumes, purge the databases, remove the environment's network and
//! the environment, say so, and forget the preview. Each step of both is a line in the job's log.

use std::time::Duration;

use axum::http::Method;
use proto::{CronRunRequest, CronTarget, Stream as LogStream};
use tokio_util::sync::CancellationToken;

use super::{Preview, State, build, comment, service};
use crate::applications::{self, Application};
use crate::cron::run::{Outcome as Exec, drive};
use crate::databases::{self, Database, DatabaseStatus};
use crate::deployments::{self, DeploymentStatus};
use crate::environments;
use crate::environments::clone::run::{Outcome, provision};
use crate::ids::{DatabaseId, PreviewId};
use crate::jobs::{Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::networks;
use crate::scope;
use crate::state::AppState;

#[cfg(not(test))]
const POLL: Duration = Duration::from_secs(3);
#[cfg(test)]
const POLL: Duration = Duration::from_millis(20);
/// How long the deploy may take.
const DEPLOY_LIMIT: Duration = Duration::from_mins(30);
/// How long the post-deploy command may run.
const COMMAND_LIMIT: Duration = Duration::from_mins(10);
/// How long a database may take to go.
#[cfg(not(test))]
const REMOVE_LIMIT: Duration = Duration::from_mins(10);
#[cfg(test)]
const REMOVE_LIMIT: Duration = Duration::from_secs(5);

fn failed(m: impl Into<String>) -> JobFailure {
	JobFailure::Failed(m.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("preview database error: {e}");
	failed("database error")
}

/// Reflects a job that failed or was cancelled onto the preview: one that was coming up is
/// `failed` with the reason. A preview being removed stays so; the sweep tries again.
///
/// # Errors
/// The database fails.
pub async fn fail(
	state: &AppState,
	target_id: i64,
	failure: &JobFailure,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
	let message = match failure {
		JobFailure::Cancelled => "cancelled".to_owned(),
		JobFailure::Failed(m) => m.clone(),
	};
	sqlx::query(
		"UPDATE previews SET state = 'failed', detail = $2
		WHERE id = $1 AND state IN ('queued', 'building')",
	)
	.bind(PreviewId(target_id))
	.bind(message.chars().take(500).collect::<String>())
	.execute(&state.db)
	.await
}

/// Runs one preview job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires; [`JobFailure::Failed`] with the reason, which
/// is also the preview's `detail`.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let preview = super::find_by_id(&state.db, PreviewId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the preview is gone"))?;
	let source = applications::find_by_id(&state.db, preview.application_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the application is gone"))?;
	if preview.state() == State::Removing {
		return teardown(state, &source, &preview, cancel, log).await;
	}
	let settings = super::settings(&state.db, source.id)
		.await
		.map_err(|e| db_failure(&e))?
		.unwrap_or_default();
	match bring_up(state, &source, &preview, &settings, cancel, log).await {
		Ok(()) => Ok(()),
		Err(JobFailure::Cancelled) => Err(JobFailure::Cancelled),
		Err(JobFailure::Failed(why)) => {
			log.push(LogStream::Stderr, &why).await;
			if let Err(e) = super::set_state(&state.db, preview.id, State::Failed, &why).await {
				tracing::warn!(preview = preview.id.0, "cannot record the failure: {e}");
			}
			if let Some(now) = super::find_by_id(&state.db, preview.id)
				.await
				.ok()
				.flatten()
			{
				comment::publish(
					state,
					&now,
					&source,
					settings.max_concurrent,
					settings.ttl_days,
				)
				.await;
				service::notify_failed(state, &source, &now, &why).await;
			}
			Err(JobFailure::Failed(why))
		}
	}
}

/// Polls `check` until it answers, `limit` passes or the job is cancelled.
async fn wait<T, F, Fut>(
	cancel: &CancellationToken,
	limit: Duration,
	mut check: F,
) -> Result<T, JobFailure>
where
	F: FnMut() -> Fut,
	Fut: std::future::Future<Output = Result<Option<T>, JobFailure>>,
{
	let deadline = tokio::time::Instant::now() + limit;
	loop {
		if let Some(v) = check().await? {
			return Ok(v);
		}
		if tokio::time::Instant::now() >= deadline {
			return Err(failed("it took too long"));
		}
		tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			() = tokio::time::sleep(POLL) => {}
		}
	}
}

async fn bring_up(
	state: &AppState,
	source: &Application,
	preview: &Preview,
	settings: &super::Settings,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let app = build::preview_app(state, preview)
		.await
		.map_err(|e| failed(e.to_string()))?
		.ok_or_else(|| failed("the preview's application is gone"))?;
	let env = preview
		.environment_id
		.ok_or_else(|| failed("the preview has no environment"))?;
	super::set_state(&state.db, preview.id, State::Building, "")
		.await
		.map_err(|e| db_failure(&e))?;
	if let Some(now) = super::find_by_id(&state.db, preview.id)
		.await
		.map_err(|e| db_failure(&e))?
	{
		comment::publish(
			state,
			&now,
			source,
			settings.max_concurrent,
			settings.ttl_days,
		)
		.await;
	}

	// Fresh databases: started, never filled from the application's.
	let dbs = databases::list_by_environment(&state.db, env)
		.await
		.map_err(|e| db_failure(&e))?;
	for db in dbs.iter().filter(|d| d.status != DatabaseStatus::Running) {
		log.push(LogStream::System, &format!("start database {}", db.name))
			.await;
		match provision(state, cancel, db).await {
			Outcome::Done(_) | Outcome::Skipped(_) => {}
			Outcome::Failed(m) => return Err(failed(format!("database {}: {m}", db.name))),
		}
	}

	// The application, from the pull request's head.
	log.push(
		LogStream::System,
		&format!("deploy {} from {}", app.name, preview.head_ref),
	)
	.await;
	let deployment = deployments::queue(state, &app, None, None)
		.await
		.map_err(|e| failed(e.to_string()))?;
	let id = deployment.id;
	wait(cancel, DEPLOY_LIMIT, || async {
		let d = deployments::find_by_id(&state.db, id)
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("the deployment is gone"))?;
		match d.status {
			DeploymentStatus::Running => Ok(Some(())),
			DeploymentStatus::Failed => Err(failed(format!(
				"the deploy failed: {}",
				d.error_message
					.unwrap_or_else(|| "no reason given".to_owned())
			))),
			DeploymentStatus::Stopped | DeploymentStatus::Crashed | DeploymentStatus::Cancelled => {
				Err(failed(format!("the deploy ended as {}", d.status.as_str())))
			}
			_ => Ok(None),
		}
	})
	.await?;

	if !settings.command.is_empty() {
		log.push(LogStream::System, &format!("run: {}", settings.command))
			.await;
		post_deploy(state, preview, &app, &settings.command, cancel, log).await?;
	}

	super::set_state(&state.db, preview.id, State::Ready, "")
		.await
		.map_err(|e| db_failure(&e))?;
	if let Some(now) = super::find_by_id(&state.db, preview.id)
		.await
		.map_err(|e| db_failure(&e))?
	{
		comment::publish(
			state,
			&now,
			source,
			settings.max_concurrent,
			settings.ttl_days,
		)
		.await;
	}
	log.push(LogStream::System, &format!("ready at {}", preview.url))
		.await;
	Ok(())
}

/// The post-deploy command, in the preview's container: migrate, seed.
async fn post_deploy(
	state: &AppState,
	preview: &Preview,
	app: &Application,
	command: &str,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let container = deployments::latest_running(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?
		.and_then(|d| d.container_id)
		.ok_or_else(|| {
			failed("the preview is not running, so there is nothing to run the command in")
		})?;
	let server = scope::server(&state.db, preview.team_id, app.server_id)
		.await
		.map_err(|e| failed(e.to_string()))?;
	let client = state
		.agents
		.client(state, &server)
		.await
		.map_err(|e| failed(e.to_string()))?;
	let request = CronRunRequest {
		run_id: preview.id.0,
		target: CronTarget::Exec {
			container,
			command: command.to_owned(),
		},
	};
	let events = client
		.stream(Method::POST, "/v1/cron/run", Some(&request))
		.await
		.map_err(|e| failed(e.to_string()))?;
	match drive(events, log, cancel, COMMAND_LIMIT).await {
		Exec::Exited(0) => Ok(()),
		Exec::Exited(code) => Err(failed(format!("the command exited with code {code}"))),
		Exec::Failed(m) => Err(failed(format!("the command could not run: {m}"))),
		Exec::TimedOut => Err(failed("the command ran past ten minutes")),
		Exec::Cancelled => Err(JobFailure::Cancelled),
	}
}

/// Takes the preview down: everything it made, then the preview itself.
async fn teardown(
	state: &AppState,
	source: &Application,
	preview: &Preview,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let team = preview.team_id;
	if let Some(app) = build::preview_app(state, preview)
		.await
		.map_err(|e| failed(e.to_string()))?
	{
		log.push(LogStream::System, &format!("remove {}", app.name))
			.await;
		applications::service::remove_service(state, team, &app).await;
	}
	if let Some(env) = preview.environment_id {
		let dbs = databases::list_by_environment(&state.db, env)
			.await
			.map_err(|e| db_failure(&e))?;
		for db in &dbs {
			log.push(LogStream::System, &format!("remove database {}", db.name))
				.await;
			remove_database(state, cancel, db).await?;
		}
		// The environment's network, on each server it was made on.
		for network in networks::list_by_environment(&state.db, team, env)
			.await
			.map_err(|e| db_failure(&e))?
		{
			if let Ok(server) = scope::server(&state.db, team, network.server_id).await
				&& let Ok(client) = state.agents.client(state, &server).await
				&& let Err(e) = client
					.send(
						Method::DELETE,
						&format!("/v1/networks/{}", network.podman_name),
					)
					.await
			{
				tracing::warn!(preview = preview.id.0, "cannot remove the network: {e}");
			}
		}
		if let Some(app) = build::preview_app(state, preview)
			.await
			.map_err(|e| failed(e.to_string()))?
		{
			applications::delete(&state.db, app.id)
				.await
				.map_err(|e| db_failure(&e))?;
		}
		environments::delete(&state.db, env).await.map_err(|e| {
			tracing::error!(preview = preview.id.0, "cannot remove the environment: {e}");
			failed("the environment still has resources")
		})?;
	}
	let settings = super::settings(&state.db, source.id)
		.await
		.map_err(|e| db_failure(&e))?
		.unwrap_or_default();
	if let Some(now) = super::find_by_id(&state.db, preview.id)
		.await
		.map_err(|e| db_failure(&e))?
	{
		comment::publish(
			state,
			&now,
			source,
			settings.max_concurrent,
			settings.ttl_days,
		)
		.await;
	}
	super::delete(&state.db, preview.id)
		.await
		.map_err(|e| db_failure(&e))?;
	log.push(LogStream::System, "removed").await;
	Ok(())
}

/// Purges a database (its data too) and waits for its row to go.
async fn remove_database(
	state: &AppState,
	cancel: &CancellationToken,
	db: &Database,
) -> Result<(), JobFailure> {
	databases::set_status(&state.db, db.id, DatabaseStatus::Deleting, None)
		.await
		.map_err(|e| db_failure(&e))?;
	databases::enqueue(state, JobKind::DatabasePurge, db)
		.await
		.map_err(|e| failed(e.to_string()))?;
	let id: DatabaseId = db.id;
	wait(cancel, REMOVE_LIMIT, || async {
		match databases::find_by_id(&state.db, id)
			.await
			.map_err(|e| db_failure(&e))?
		{
			None => Ok(Some(())),
			Some(row) if row.status == DatabaseStatus::Failed => Err(failed(format!(
				"database {} could not be removed: {}",
				row.name,
				row.status_message.unwrap_or_default()
			))),
			Some(_) => Ok(None),
		}
	})
	.await
}
