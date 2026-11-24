//! The Application move job and the Server drain job.
//!
//! A move does its own work (copying the image, stopping containers, the switch, removing
//! the old containers) and queues the jobs that already do the rest: a volume backup on the
//! old server, a volume restore and a deploy on the new one. It waits for each, the way an
//! environment clone does, so its own lock (`move:<app>`) never blocks theirs (`app:<app>`).
//!
//! A drain moves one application at a time through such a job and waits for it before the
//! next, so only one of a server's applications is in flight at once.

use std::future::Future;
use std::time::Duration;

use axum::http::Method as HttpMethod;
use proto::Stream as LogStream;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::decide::{self, Method};
use super::service;
use super::{
	DrainStatus, Item, ItemKind, Move, MoveStatus, NewMove, Outcome, drain_status, find_drain,
	find_move, finish_move, save_drain, set_step, switch, switch_back,
};
use crate::agents::AgentClient;
use crate::applications::{self, Application};
use crate::backups::BackupStatus;
use crate::deployments::{self, Deployment, DeploymentStatus};
use crate::error::ServerError;
use crate::ids::{ApplicationId, DrainId, MoveId, TeamId, UserId, VolumeBackupId};
use crate::jobs::{self, Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::servers::{Server, ServerStatus};
use crate::state::AppState;
use crate::volume_backups::subject::{self, Vol};
use crate::volume_backups::{self, RestoreMode, RestoreState};
use crate::{builds, db, scope, volumes};

/// How long one step may wait for the job it queued.
const STEP_LIMIT: Duration = Duration::from_hours(1);
const POLL: Duration = Duration::from_secs(2);

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("move job database error: {e}");
	failed("database error")
}

/// Polls `check` until it gives a value, fails, the limit passes or `cancel` fires.
async fn wait<T, F, Fut>(cancel: &CancellationToken, mut check: F) -> Result<T, JobFailure>
where
	F: FnMut() -> Fut,
	Fut: Future<Output = Result<Option<T>, JobFailure>>,
{
	let deadline = tokio::time::Instant::now() + STEP_LIMIT;
	loop {
		if let Some(v) = check().await? {
			return Ok(v);
		}
		if tokio::time::Instant::now() >= deadline {
			return Err(failed("a step took longer than an hour"));
		}
		tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			() = tokio::time::sleep(POLL) => {}
		}
	}
}

/// The team of an application, through its project: the job runs without a team context.
async fn team_of(state: &AppState, app: &Application) -> Result<TeamId, JobFailure> {
	sqlx::query_scalar::<_, TeamId>("SELECT team_id FROM projects WHERE id = $1")
		.bind(app.project_id)
		.fetch_optional(&state.db)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the project is gone"))
}

async fn server(
	state: &AppState,
	team: TeamId,
	id: Option<crate::ids::ServerId>,
	which: &str,
) -> Result<Server, JobFailure> {
	let Some(id) = id else {
		return Err(failed(format!("the {which} server no longer exists")));
	};
	scope::server(&state.db, team, id)
		.await
		.map_err(|e| match e {
			ServerError::Sqlx(e) => db_failure(&e),
			_ => failed(format!("the {which} server no longer exists")),
		})
}

async fn client(state: &AppState, server: &Server) -> Result<AgentClient, JobFailure> {
	state.agents.client(state, server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		failed(format!("the agent of {} is unavailable", server.name))
	})
}

/// Says what the move does now, on its row and in its log.
async fn step(state: &AppState, mv: MoveId, log: &mut JobLog, text: &str) {
	if let Err(e) = set_step(&state.db, mv, text).await {
		warn!(%mv, "cannot record the step of a move: {e}");
	}
	log.push(LogStream::System, text).await;
}

/// The containers of `running` on its server: the deployment's own and its replicas'.
async fn containers(client: &AgentClient, app: ApplicationId, running: &Deployment) -> Vec<String> {
	let mut out: Vec<String> = running.container_id.iter().cloned().collect();
	if let Ok(replicas) = crate::replicas::service::names(client, app.0, running.id.0).await {
		for r in replicas {
			if !out.contains(&r.id) {
				out.push(r.id);
			}
		}
	}
	out
}

/// Stops each container in turn. On failure the ones already stopped are started again.
async fn stop_all(client: &AgentClient, ids: &[String]) -> Result<Vec<String>, JobFailure> {
	let mut stopped = Vec::new();
	for id in ids {
		if let Err(e) = client
			.send(HttpMethod::POST, &format!("/v1/containers/{id}/stop"))
			.await
		{
			start_all(client, &stopped).await;
			return Err(failed(format!("could not stop the application: {e}")));
		}
		stopped.push(id.clone());
	}
	Ok(stopped)
}

/// Starts the containers again, last stopped first; best effort, each failure logged.
async fn start_all(client: &AgentClient, ids: &[String]) {
	for id in ids.iter().rev() {
		if let Err(e) = client
			.send(HttpMethod::POST, &format!("/v1/containers/{id}/start"))
			.await
		{
			warn!(container = %id, "cannot start a container again after a failed move: {e}");
		}
	}
}

/// Archives one volume of the application on its current server through `target` and waits
/// for the archive.
async fn archive(
	state: &AppState,
	cancel: &CancellationToken,
	team: TeamId,
	app: &Application,
	volume: &volumes::Volume,
	target: crate::ids::BackupTargetId,
	user: Option<UserId>,
) -> Result<VolumeBackupId, JobFailure> {
	let subject = subject::resolve(
		state,
		team,
		app.project_id,
		&Vol::App {
			app: app.id,
			volume: volume.id,
		},
	)
	.await
	.map_err(|e| failed(e.to_string()))?;
	let policy = volume_backups::ensure_policy(&state.db, &subject)
		.await
		.map_err(|e| db_failure(&e))?;
	if volume_backups::busy(&state.db, policy.id)
		.await
		.map_err(|e| db_failure(&e))?
	{
		return Err(failed(format!(
			"a backup or restore of the volume {} is already running",
			volume.mount_path
		)));
	}
	let backup = volume_backups::queue_backup(state, &subject, &policy, target, user)
		.await
		.map_err(|e| db_failure(&e))?;
	wait(cancel, || async {
		let row = volume_backups::backup_by_id(&state.db, backup.id)
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("the archive is gone"))?;
		match row.status {
			BackupStatus::Succeeded => Ok(Some(row.id)),
			BackupStatus::Failed => Err(failed(format!(
				"archiving {} failed: {}",
				volume.mount_path,
				row.error.unwrap_or_else(|| "no reason given".to_owned())
			))),
			_ => Ok(None),
		}
	})
	.await
}

/// Restores an archive into the volume of the same name on the application's (new) server,
/// emptying it first, and waits.
async fn restore(
	state: &AppState,
	cancel: &CancellationToken,
	app: &Application,
	volume: &str,
	backup: VolumeBackupId,
	user: UserId,
) -> Result<(), JobFailure> {
	let row = volume_backups::insert_restore(&state.db, backup, RestoreMode::Same, volume, user)
		.await
		.map_err(|e| db_failure(&e))?;
	jobs::enqueue(
		state,
		JobKind::VolumeRestore,
		app.server_id,
		row.id.0,
		format!("app:{}", app.id),
	)
	.await
	.map_err(|e| db_failure(&e))?;
	wait(cancel, || async {
		let r = volume_backups::restore_by_id(&state.db, row.id)
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("the restore is gone"))?;
		match r.status {
			RestoreState::Succeeded => Ok(Some(())),
			RestoreState::Failed | RestoreState::Cancelled => Err(failed(format!(
				"restoring {volume} failed: {}",
				r.error.unwrap_or_else(|| "no reason given".to_owned())
			))),
			_ => Ok(None),
		}
	})
	.await
}

/// Waits for a deployment to run, or says why it did not.
async fn until_running(
	state: &AppState,
	cancel: &CancellationToken,
	deployment: &Deployment,
) -> Result<(), JobFailure> {
	wait(cancel, || async {
		let d = deployments::find_by_id(&state.db, deployment.id)
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("the deployment is gone"))?;
		match d.status {
			DeploymentStatus::Running => Ok(Some(())),
			DeploymentStatus::Failed | DeploymentStatus::Cancelled | DeploymentStatus::Crashed => {
				Err(failed(format!(
					"the deploy on the new server failed: {}",
					d.error_message
						.unwrap_or_else(|| d.status.as_str().to_owned())
				)))
			}
			_ => Ok(None),
		}
	})
	.await
}

/// What happens once the application points at the target: the volumes are restored, and it
/// is started when it ran before.
#[allow(clippy::too_many_arguments, reason = "one step of one job")]
async fn on_target(
	state: &AppState,
	cancel: &CancellationToken,
	log: &mut JobLog,
	mv: &Move,
	to: &Server,
	archives: &[(String, VolumeBackupId)],
	starts: bool,
	reuse: Option<&Deployment>,
) -> Result<(), JobFailure> {
	let app = applications::find_by_id(&state.db, mv.application_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the application is gone"))?;
	if !archives.is_empty() {
		let Some(user) = mv.requested_by else {
			return Err(failed(
				"the person who asked for the move no longer exists, so nobody restores the volumes",
			));
		};
		for (volume, backup) in archives {
			step(
				state,
				mv.id,
				log,
				&format!("restoring {volume} on {}", to.name),
			)
			.await;
			restore(state, cancel, &app, volume, *backup, user).await?;
		}
	}
	if !starts {
		return Ok(());
	}
	step(state, mv.id, log, &format!("starting it on {}", to.name)).await;
	let deployment = deployments::queue_for_move(state, &app, mv.requested_by, reuse)
		.await
		.map_err(|e| failed(e.to_string()))?;
	if let Err(e) = sqlx::query("UPDATE application_moves SET deployment_id = $2 WHERE id = $1")
		.bind(mv.id)
		.bind(deployment.id)
		.execute(&state.db)
		.await
	{
		warn!(move_id = %mv.id, "cannot record the deployment of a move: {e}");
	}
	log.push(
		LogStream::System,
		&format!("deployment #{} follows on {}", deployment.id, to.name),
	)
	.await;
	until_running(state, cancel, &deployment).await
}

/// After the move: the application leaves the networks of the old server, and its old
/// containers are removed there. Best effort; what failed is returned for the step line.
async fn clean_up(state: &AppState, app: ApplicationId, from: &Server) -> Option<String> {
	if let Err(e) = sqlx::query(
		"DELETE FROM resource_networks rn USING networks n
		WHERE rn.network_id = n.id AND n.server_id = $2
			AND rn.resource_kind = 'application' AND rn.resource_id = $1",
	)
	.bind(app)
	.bind(from.id)
	.execute(&state.db)
	.await
	{
		warn!(application = %app, "cannot drop the old network memberships: {e}");
	}
	if from.status != ServerStatus::Active {
		return Some(format!(
			"{} did not answer, so its old container may still be there: remove it once the server is back",
			from.name
		));
	}
	let client = match state.agents.client(state, from).await {
		Ok(c) => c,
		Err(e) => {
			return Some(format!(
				"could not reach {} to remove the old container: {e}",
				from.name
			));
		}
	};
	match client
		.send(HttpMethod::DELETE, &format!("/v1/services/app-{app}"))
		.await
	{
		Ok(()) => None,
		Err(e) => Some(format!(
			"could not remove the old container on {}: {e}",
			from.name
		)),
	}
}

/// Runs one Application move job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`] with what
/// went wrong; by then the application is back on its old server and what was stopped runs
/// again.
#[allow(
	clippy::too_many_lines,
	reason = "the steps of a move read best in order"
)]
pub async fn run_move(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let mv = find_move(&state.db, MoveId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the move is gone"))?;
	let app = applications::find_by_id(&state.db, mv.application_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the application is gone"))?;
	let team = team_of(state, &app).await?;
	let from = server(state, team, mv.from_server_id, "old").await?;
	let to = server(state, team, mv.to_server_id, "new").await?;
	if app.server_id != from.id {
		return Err(failed(format!(
			"{} no longer runs on {}",
			app.name, from.name
		)));
	}
	step(state, mv.id, log, "checking").await;

	// Judged again now: things change between the request and the turn of the job.
	let draining = super::draining_servers(&state.db, team)
		.await
		.map_err(|e| db_failure(&e))?;
	let mut facts = service::facts(
		&state.db,
		&app,
		Some(&from),
		Some(&to),
		mv.backup_target_id.is_some(),
		&draining,
	)
	.await
	.map_err(|e| failed(e.to_string()))?;
	// This move is the one in flight.
	facts.moving = false;
	let verdict = decide::decide(&facts);
	if !verdict.movable() {
		return Err(failed(verdict.refusals.join(" ")));
	}
	let method = verdict.method;
	if method != mv.method {
		log.push(
			LogStream::System,
			&format!(
				"{} since the move was asked for: {} instead",
				if method == Method::Rebuild {
					format!("{} stopped answering", from.name)
				} else {
					"things changed".to_owned()
				},
				method.label().to_lowercase()
			),
		)
		.await;
		let _ = sqlx::query("UPDATE application_moves SET method = $2 WHERE id = $1")
			.bind(mv.id)
			.bind(method)
			.execute(&state.db)
			.await;
	}
	log.push(
		LogStream::System,
		&format!(
			"moving {} from {} to {}: {}",
			app.name,
			from.name,
			to.name,
			method.label().to_lowercase()
		),
	)
	.await;

	let running = deployments::latest_running(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?;
	// What the target runs: the live image, or the newest one when nothing runs (copied so a
	// restart there works). A rebuild has none.
	let image = match running.as_ref().filter(|d| d.image_tag.is_some()) {
		_ if method == Method::Rebuild => None,
		Some(d) => Some(d.clone()),
		None => deployments::latest_with_image(&state.db, app.id)
			.await
			.map_err(|e| db_failure(&e))?,
	};

	let from_client = if method == Method::Rebuild {
		None
	} else {
		Some(client(state, &from).await?)
	};
	if let (Some(from_client), Some(tag)) = (
		from_client.as_ref(),
		image.as_ref().and_then(|d| d.image_tag.as_deref()),
	) {
		step(
			state,
			mv.id,
			log,
			&format!("copying the image to {}", to.name),
		)
		.await;
		let to_client = client(state, &to).await?;
		builds::delivery::deliver(from_client, &to_client, tag, cancel, log).await?;
	}

	// Volumes: stop, archive each through the destination.
	let mut stopped: Vec<String> = Vec::new();
	let mut archives: Vec<(String, VolumeBackupId)> = Vec::new();
	if method == Method::StopCopy {
		let target = mv
			.backup_target_id
			.ok_or_else(|| failed("no backup destination was chosen for the volume data"))?;
		let from_client = from_client
			.as_ref()
			.ok_or_else(|| failed("the old server is unavailable"))?;
		if let Some(live) = &running {
			step(
				state,
				mv.id,
				log,
				&format!("stopping {} on {}", app.name, from.name),
			)
			.await;
			let ids = containers(from_client, app.id, live).await;
			stopped = stop_all(from_client, &ids).await?;
		}
		for volume in volumes::list(&state.db, app.id)
			.await
			.map_err(|e| db_failure(&e))?
		{
			step(
				state,
				mv.id,
				log,
				&format!("archiving {} ({})", volume.mount_path, volume.name),
			)
			.await;
			match archive(state, cancel, team, &app, &volume, target, mv.requested_by).await {
				Ok(backup) => archives.push((volume.name.clone(), backup)),
				Err(e) => {
					start_all(from_client, &stopped).await;
					return Err(e);
				}
			}
		}
	}

	// The switch.
	step(
		state,
		mv.id,
		log,
		&format!("pointing {} at {}", app.name, to.name),
	)
	.await;
	let switched = switch(&state.db, mv.id, app.id, from.id, to.id).await;
	let refused = match switched {
		Ok(true) => None,
		Ok(false) => Some(failed(format!(
			"{} no longer runs on {}",
			app.name, from.name
		))),
		Err(e) if db::is_unique_violation(&e, applications::SLUG_CONSTRAINT) => {
			Some(failed(format!(
				"another application on {} is called `{}` now",
				to.name, app.slug
			)))
		}
		Err(e) => Some(db_failure(&e)),
	};
	if let Some(refused) = refused {
		if let Some(c) = from_client.as_ref() {
			start_all(c, &stopped).await;
		}
		return Err(refused);
	}

	if let Err(e) = on_target(
		state,
		cancel,
		log,
		&mv,
		&to,
		&archives,
		verdict.starts,
		image.as_ref(),
	)
	.await
	{
		log.push(
			LogStream::Stderr,
			&format!("putting {} back on {}", app.name, from.name),
		)
		.await;
		if let Err(back) = switch_back(&state.db, mv.id, app.id, from.id, to.id).await {
			tracing::error!(move_id = %mv.id, "cannot put the application back: {back}");
		}
		if let Some(c) = from_client.as_ref() {
			start_all(c, &stopped).await;
		}
		return Err(e);
	}

	step(
		state,
		mv.id,
		log,
		&format!("removing the old container on {}", from.name),
	)
	.await;
	let note = clean_up(state, app.id, &from).await;
	if let Some(note) = &note {
		log.push(LogStream::Stderr, note).await;
	}
	let done = match &note {
		Some(note) => format!("on {}; {note}", to.name),
		None => format!("on {}", to.name),
	};
	finish_move(&state.db, mv.id, MoveStatus::Succeeded, &done, None)
		.await
		.map_err(|e| db_failure(&e))?;
	log.push(LogStream::System, &format!("{} is {done}", app.name))
		.await;
	Ok(())
}

// --- drains -----------------------------------------------------------------------------------

async fn save(
	state: &AppState,
	drain: DrainId,
	status: DrainStatus,
	report: &[Item],
) -> Result<(), JobFailure> {
	save_drain(&state.db, drain, status, report)
		.await
		.map_err(|e| db_failure(&e))
}

/// Moves one application of the drain and waits for the move. The outcome and its detail.
async fn move_one(
	state: &AppState,
	cancel: &CancellationToken,
	team: TeamId,
	drain: &super::Drain,
	from: &Server,
	to: &Server,
	item: &mut Item,
) -> Result<(), JobFailure> {
	let Some(app) = applications::find_by_id(&state.db, ApplicationId(item.id))
		.await
		.map_err(|e| db_failure(&e))?
		.filter(|a| a.server_id == from.id)
	else {
		item.outcome = Outcome::Skipped;
		item.detail = Some(format!("no longer on {}", from.name));
		return Ok(());
	};
	let draining = super::draining_servers(&state.db, team)
		.await
		.map_err(|e| db_failure(&e))?;
	let facts = service::facts(
		&state.db,
		&app,
		Some(from),
		Some(to),
		drain.backup_target_id.is_some(),
		&draining,
	)
	.await
	.map_err(|e| failed(e.to_string()))?;
	let verdict = decide::decide(&facts);
	item.method = Some(verdict.method);
	if !verdict.movable() {
		item.outcome = Outcome::Kept;
		item.detail = Some(verdict.refusals.join(" "));
		return Ok(());
	}
	let mv = match service::queue(
		state,
		&NewMove {
			application: app.id,
			from,
			to,
			method: verdict.method,
			backup_target: drain.backup_target_id.filter(|_| facts.volumes > 0),
			drain: Some(drain.id),
			requested_by: drain.requested_by,
		},
	)
	.await
	{
		Ok(mv) => mv,
		Err(e) => {
			item.outcome = Outcome::Failed;
			item.detail = Some(e.to_string());
			return Ok(());
		}
	};
	item.outcome = Outcome::Moving;
	item.move_id = Some(mv.id);
	let ended = wait(cancel, || async {
		let row = find_move(&state.db, mv.id)
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("the move is gone"))?;
		Ok((!row.status.in_flight()).then_some(row))
	})
	.await?;
	if ended.status == MoveStatus::Succeeded {
		item.outcome = Outcome::Moved;
		item.detail = Some(ended.step);
	} else {
		item.outcome = Outcome::Failed;
		item.detail = Some(ended.error.unwrap_or(ended.step));
	}
	Ok(())
}

/// Runs one Server drain job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when the drain is stopped (the move in flight goes on), otherwise
/// [`JobFailure::Failed`] when a move failed; what stays on the server by design is not a
/// failure, the report says why.
#[allow(
	clippy::too_many_lines,
	reason = "the loop over the report reads best in one piece"
)]
pub async fn run_drain(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let drain = find_drain(&state.db, DrainId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the drain is gone"))?;
	let from = server(state, drain.team_id, Some(drain.server_id), "drained").await?;
	let to = server(state, drain.team_id, drain.target_server_id, "target").await?;

	// What is on the server now; the report made at the request may be stale.
	let mut report: Vec<Item> = service::inventory(
		&state.db,
		drain.team_id,
		&from,
		Some(&to),
		drain.backup_target_id.is_some(),
	)
	.await
	.map_err(|e| failed(e.to_string()))?
	.into_iter()
	.map(|f| f.item)
	.collect();
	save(state, drain.id, DrainStatus::Running, &report).await?;
	log.push(
		LogStream::System,
		&format!(
			"draining {} into {}: {} things found, {} to move",
			from.name,
			to.name,
			report.len(),
			report
				.iter()
				.filter(|i| i.outcome == Outcome::Pending)
				.count()
		),
	)
	.await;

	for i in 0..report.len() {
		if report[i].outcome != Outcome::Pending {
			continue;
		}
		if cancel.is_cancelled() {
			for rest in report[i..]
				.iter_mut()
				.filter(|r| r.outcome == Outcome::Pending)
			{
				rest.outcome = Outcome::Skipped;
				rest.detail = Some("the drain was stopped before it got here".to_owned());
			}
			save(state, drain.id, DrainStatus::Cancelled, &report).await?;
			return Err(JobFailure::Cancelled);
		}
		log.push(
			LogStream::System,
			&format!("{} {}", report[i].kind.label(), report[i].name),
		)
		.await;
		match report[i].kind {
			ItemKind::Application => {
				report[i].outcome = Outcome::Moving;
				save(state, drain.id, DrainStatus::Running, &report).await?;
				let mut item = report[i].clone();
				let result =
					move_one(state, cancel, drain.team_id, &drain, &from, &to, &mut item).await;
				report[i] = item;
				if let Err(JobFailure::Cancelled) = result {
					report[i].detail = Some(
						"the drain was stopped while this one was moving; its move goes on"
							.to_owned(),
					);
					for rest in report[i + 1..]
						.iter_mut()
						.filter(|r| r.outcome == Outcome::Pending)
					{
						rest.outcome = Outcome::Skipped;
						rest.detail = Some("the drain was stopped before it got here".to_owned());
					}
					save(state, drain.id, DrainStatus::Cancelled, &report).await?;
					return Err(JobFailure::Cancelled);
				}
				result?;
			}
			ItemKind::CronTask => {
				let moved = sqlx::query(
					"UPDATE cron_tasks SET server_id = $3, updated_at = NOW()
					WHERE id = $1 AND server_id = $2 AND application_id IS NULL",
				)
				.bind(report[i].id)
				.bind(from.id)
				.bind(to.id)
				.execute(&state.db)
				.await
				.map_err(|e| db_failure(&e))?
				.rows_affected() == 1;
				report[i].outcome = if moved {
					Outcome::Moved
				} else {
					Outcome::Skipped
				};
				report[i].detail = Some(if moved {
					format!("its runs start on {}", to.name)
				} else {
					format!("no longer on {}", from.name)
				});
			}
			_ => {}
		}
		log.push(
			if report[i].outcome == Outcome::Failed {
				LogStream::Stderr
			} else {
				LogStream::System
			},
			&format!(
				"  {:?}{}",
				report[i].outcome,
				report[i]
					.detail
					.as_ref()
					.map(|d| format!(": {d}"))
					.unwrap_or_default()
			)
			.to_lowercase(),
		)
		.await;
		save(state, drain.id, DrainStatus::Running, &report).await?;
	}

	let status = drain_status(&report);
	save(state, drain.id, status, &report).await?;
	let failed_count = report
		.iter()
		.filter(|i| i.outcome == Outcome::Failed)
		.count();
	let kept = report.iter().filter(|i| i.outcome == Outcome::Kept).count();
	log.push(
		LogStream::System,
		&format!(
			"done: {} moved, {kept} kept on {}, {failed_count} failed",
			report
				.iter()
				.filter(|i| i.outcome == Outcome::Moved)
				.count(),
			from.name
		),
	)
	.await;
	if failed_count > 0 {
		return Err(failed(format!(
			"{failed_count} of {} moves failed; see the drain report",
			report
				.iter()
				.filter(|i| i.kind == ItemKind::Application)
				.count()
		)));
	}
	Ok(())
}
