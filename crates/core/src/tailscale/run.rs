//! The two jobs of a tailnet exposure: start the sidecar and wait until its node has joined,
//! and log the node out and remove the sidecar. Every `Err` is written onto the exposure row
//! by the worker (`jobs::fail_target`), so success paths only move the row forward.

use std::time::{Duration, Instant};

use axum::http::Method;
use futures_util::StreamExt;
use proto::{Event, ExecRequest, ExecResult, Ingress, Stream};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::ExposureStatus;
use super::spec::{self, Node, Sidecar};
use crate::agents::AgentClient;
use crate::applications::{self, Application};
use crate::audit::{self, Diff, Target};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::jobs::{Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::state::AppState;
use crate::{networks, servers};

/// How long the node may take to join the tailnet: image pull and login included.
const JOIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Pause between two looks at the node's state.
const JOIN_POLL: Duration = Duration::from_secs(2);
/// `NeedsLogin` shows briefly before the auth key is used; this long after it, a node that
/// still needs a login was refused its key.
const LOGIN_GRACE: Duration = Duration::from_secs(40);

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("tailscale job database error: {e}");
	failed("database error")
}

/// Keeps the agent's own client-facing messages and hides the rest.
fn agent_failure(e: ServerError) -> JobFailure {
	match e {
		ServerError::Conflict(m) | ServerError::Validation(m) => failed(m),
		e => {
			warn!("agent request failed: {e}");
			failed("server agent unavailable")
		}
	}
}

/// Runs one `Tailscale*` job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`] with a
/// message safe to show the application's owner.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let app = applications::find_by_id(&state.db, ApplicationId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("application not found"))?;
	let server = servers::find(&state.db, app.server_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("target server not found"))?;
	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;
	match job.kind {
		JobKind::TailscaleStart => start(state, job, cancel, log, &app, &client).await,
		JobKind::TailscaleRemove => remove(state, log, &app, &client).await,
		_ => Err(failed("not a tailscale job")),
	}
}

async fn team_of(state: &AppState, project: ProjectId) -> Result<TeamId, JobFailure> {
	sqlx::query_scalar::<_, TeamId>("SELECT team_id FROM projects WHERE id = $1")
		.bind(project)
		.fetch_optional(&state.db)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("project not found"))
}

async fn start(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
	app: &Application,
	client: &AgentClient,
) -> Result<(), JobFailure> {
	let exposure = super::find(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the exposure was removed"))?;
	if exposure.status == ExposureStatus::Removing {
		return Err(failed("the exposure is being removed"));
	}
	let team = team_of(state, app.project_id).await?;
	let settings = super::settings(&state.db, team)
		.await
		.map_err(|e| db_failure(&e))?;
	let blob = super::auth_key_blob(&state.db, team)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("no Tailscale auth key is stored; add one in Settings"))?;
	let auth_key = state.secrets.decrypt(&blob).map_err(|e| {
		tracing::error!(
			team = team.0,
			"failed to decrypt the Tailscale auth key: {e}"
		);
		failed("the stored auth key cannot be read; add it again in Settings")
	})?;

	let network = networks::ensure_environment(&state.db, app.environment_id, app.server_id)
		.await
		.map_err(|e| db_failure(&e))?;
	if network.internal {
		// An internal network has no way out, and the node has to reach the control server.
		return Err(failed(
			"this environment's network has no outbound access, so the sidecar cannot reach the \
			 Tailscale control server",
		));
	}
	let port = u16::try_from(app.container_port)
		.map_err(|_| failed("the application's port is not valid"))?;
	let spec = spec::build_spec(&Sidecar {
		application: app.id,
		hostname: &exposure.hostname,
		container_port: port,
		auth_key: &auth_key,
		login_server: settings.login_server.as_deref(),
		tags: &settings.tags,
		network: proto::NetworkAttach {
			name: network.podman_name,
			aliases: Vec::new(),
			internal: false,
		},
		ingress: Ingress {
			http_port: state.config.ingress_port,
			https_port: state.config.https_port,
			acme_email: state.config.acme_email.clone(),
		},
		instance_id: job.id.0,
	});
	super::set_status(&state.db, app.id, ExposureStatus::Starting, None)
		.await
		.map_err(|e| db_failure(&e))?;
	log.push(
		Stream::System,
		&format!("starting the Tailscale sidecar as {}", exposure.hostname),
	)
	.await;
	let container = deploy(state, cancel, log, app, client, &spec).await?;
	wait_joined(
		state,
		cancel,
		log,
		app.id,
		client,
		&container,
		JOIN_TIMEOUT,
		JOIN_POLL,
		LOGIN_GRACE,
	)
	.await
}

async fn deploy(
	state: &AppState,
	cancel: &CancellationToken,
	log: &mut JobLog,
	app: &Application,
	client: &AgentClient,
	spec: &proto::DeploySpec,
) -> Result<String, JobFailure> {
	let mut events = client
		.stream(Method::POST, "/v1/deploy", Some(spec))
		.await
		.map_err(agent_failure)?;
	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &line).await,
			Some(Ok(Event::Deployed { container_id, .. })) => {
				super::set_container(&state.db, app.id, &container_id)
					.await
					.map_err(|e| db_failure(&e))?;
				return Ok(container_id);
			}
			Some(Ok(Event::Failed { message })) => return Err(failed(message)),
			Some(Ok(_)) => {}
			Some(Err(e)) => {
				warn!(application = app.id.0, "sidecar deploy stream failed: {e}");
				return Err(failed("lost connection to server agent"));
			}
			None => return Err(failed("lost connection to server agent")),
		}
	}
}

async fn exec(
	client: &AgentClient,
	container: &str,
	cmd: Vec<String>,
) -> Result<ExecResult, ServerError> {
	client
		.json(
			Method::POST,
			&format!("/v1/containers/{container}/exec"),
			Some(&ExecRequest {
				cmd,
				timeout_s: Some(15),
			}),
		)
		.await
}

/// Looks at the node until it is on the tailnet, then records its name. A node that still asks
/// for a login after `grace` was refused its key, which is reported as such.
#[allow(
	clippy::too_many_arguments,
	reason = "the timings are what tests shorten"
)]
pub(super) async fn wait_joined(
	state: &AppState,
	cancel: &CancellationToken,
	log: &mut JobLog,
	app: ApplicationId,
	client: &AgentClient,
	container: &str,
	timeout: Duration,
	poll: Duration,
	grace: Duration,
) -> Result<(), JobFailure> {
	log.push(Stream::System, "waiting for the node to join the tailnet")
		.await;
	let started = Instant::now();
	let mut last = String::from("no answer yet");
	loop {
		match exec(client, container, spec::status_cmd()).await {
			Ok(result) => match spec::parse_status(&result.output) {
				Some(Node::Running { dns_name }) => {
					super::set_running(&state.db, app, &dns_name)
						.await
						.map_err(|e| db_failure(&e))?;
					log.push(Stream::System, &format!("the node is up as {dns_name}"))
						.await;
					return Ok(());
				}
				Some(Node::NeedsLogin) => {
					"NeedsLogin".clone_into(&mut last);
					if started.elapsed() >= grace {
						return Err(failed(
							"Tailscale did not accept the auth key: it may be expired, already \
							 used, or revoked. Create a new key and save it in Settings.",
						));
					}
				}
				Some(Node::Other(state_name)) => last = state_name,
				None => "no status yet".clone_into(&mut last),
			},
			// The daemon is not up yet.
			Err(ServerError::Validation(_) | ServerError::Conflict(_)) => {}
			Err(e) => return Err(agent_failure(e)),
		}
		if started.elapsed() >= timeout {
			return Err(failed(format!(
				"the node did not join the tailnet in time (last state: {last})"
			)));
		}
		tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			() = tokio::time::sleep(poll) => {}
		}
	}
}

async fn remove(
	state: &AppState,
	log: &mut JobLog,
	app: &Application,
	client: &AgentClient,
) -> Result<(), JobFailure> {
	let exposure = super::find(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?;
	let Some(exposure) = exposure else {
		return Ok(());
	};
	log.push(
		Stream::System,
		"logging the node out and removing the sidecar",
	)
	.await;
	let notes = teardown(client, app.id, exposure.container_id.as_deref()).await;
	for note in &notes {
		log.push(Stream::System, note).await;
	}
	// The sidecar and its volume are the only things to clean; if the agent could not be
	// told, the row stays so the removal can be tried again.
	if notes.iter().any(|n| n.starts_with("could not remove")) {
		return Err(failed(
			"the sidecar could not be removed; try again once the server answers",
		));
	}
	super::delete(&state.db, app.id)
		.await
		.map_err(|e| db_failure(&e))?;
	if let Ok(team) = team_of(state, app.project_id).await {
		audit::record(
			&state.db,
			&audit::system(team),
			"tailscale.removed",
			Target::new("application", app.id)
				.named(&app.slug)
				.in_project(app.project_id),
			Diff::new().set("hostname", exposure.hostname),
		)
		.await;
	}
	Ok(())
}

/// Logs the node out (so it leaves the tailnet's machine list instead of sitting there offline),
/// then removes the sidecar and its state volume. Returns what happened, one line each; a line
/// starting with `could not remove` means the sidecar may still be there.
pub async fn teardown(
	client: &AgentClient,
	app: ApplicationId,
	container: Option<&str>,
) -> Vec<String> {
	let mut notes = Vec::new();
	match container {
		Some(container) => match exec(client, container, spec::logout_cmd()).await {
			Ok(r) if r.exit_code == 0 => notes.push("the node is logged out".to_owned()),
			Ok(r) => notes.push(format!(
				"logout exited with {}; remove the node in the Tailscale admin console if it stays \
				 listed",
				r.exit_code
			)),
			Err(e) => notes.push(format!(
				"could not log the node out ({e}); remove it in the Tailscale admin console if it \
				 stays listed"
			)),
		},
		None => notes.push("the sidecar never started, so there is no node to log out".to_owned()),
	}
	if let Err(e) = client
		.send(
			Method::DELETE,
			&format!("/v1/services/{}", spec::service(app)),
		)
		.await
	{
		notes.push(format!("could not remove the sidecar container: {e}"));
		return notes;
	}
	match client
		.send(
			Method::DELETE,
			&format!("/v1/volumes/{}", spec::volume(app)),
		)
		.await
	{
		Ok(()) => notes.push("the sidecar and its state are removed".to_owned()),
		Err(e) => notes.push(format!("could not remove the sidecar's state volume: {e}")),
	}
	notes
}
