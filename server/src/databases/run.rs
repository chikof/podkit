//! Database jobs: provision (deploy the engine container), start, stop and
//! remove. Every `Err` is mapped onto the database row by the worker
//! (`jobs::fail_target`), so success paths only move the row forward.

use axum::http::Method;
use futures_util::StreamExt;
use proto::{DeploySpec, Event, Ingress, PublishedPort, Source, Strategy, Stream, VolumeMount};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{Database, DatabaseStatus, engines};
use crate::agents::AgentClient;
use crate::databases;
use crate::error::ServerError;
use crate::ids::DatabaseId;
use crate::jobs::{Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::servers::{self, Server};
use crate::state::AppState;

const LOST_CONNECTION: &str = "lost connection to server agent";

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("database job database error: {e}");
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

/// Runs one Database* job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`] with a message safe to show the database owner.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let database = databases::find_by_id(&state.db, DatabaseId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("database not found"))?;
	let remove = matches!(job.kind, JobKind::DatabaseRemove | JobKind::DatabasePurge);
	if database.status == DatabaseStatus::Deleting && !remove {
		return Err(failed("database is being deleted"));
	}

	let server = servers::find(&state.db, database.server_id)
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
		JobKind::DatabaseProvision => {
			provision(state, job, cancel, log, &database, &server, &client).await
		}
		JobKind::DatabaseStart => set_running(state, log, &database, &client, true).await,
		JobKind::DatabaseStop => set_running(state, log, &database, &client, false).await,
		JobKind::DatabaseRemove => remove_database(state, log, &database, &client, false).await,
		JobKind::DatabasePurge => remove_database(state, log, &database, &client, true).await,
		_ => Err(failed("not a database job")),
	}
}

async fn provision(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
	database: &Database,
	server: &Server,
	client: &AgentClient,
) -> Result<(), JobFailure> {
	// Leaves `running` so the status sync does not flag the container this deploy replaces.
	databases::set_status(&state.db, database.id, DatabaseStatus::Pending, None)
		.await
		.map_err(|e| db_failure(&e))?;

	let spec = build_spec(state, job, database)?;
	let mut events = client
		.stream(Method::POST, "/v1/deploy", Some(&spec))
		.await
		.map_err(|e| {
			warn!(server = %server.id, "failed to start database deploy stream: {e}");
			failed("server agent unavailable")
		})?;

	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &line).await,
			Some(Ok(Event::Deployed {
				container_id,
				image,
				..
			})) => {
				sqlx::query!(
					"UPDATE databases SET container_id = $2, status = 'running', status_message = NULL,
						updated_at = NOW() WHERE id = $1",
					database.id as DatabaseId,
					container_id,
				)
				.execute(&state.db)
				.await
				.map_err(|e| db_failure(&e))?;
				crate::scanning::queue(
					state,
					database.server_id,
					crate::scanning::Target {
						kind: crate::scanning::ResourceKind::Database,
						resource_id: database.id.0,
						image_ref: &image,
						deployment: None,
					},
				)
				.await;
				return Ok(());
			}
			Some(Ok(Event::Failed { message })) => return Err(JobFailure::Failed(message)),
			Some(Ok(
				Event::Phase { .. }
				| Event::Progress { .. }
				| Event::DumpWritten { .. }
				| Event::Done,
			)) => {}
			Some(Err(e)) => {
				warn!(
					database = database.id.0,
					"database deploy stream failed: {e}"
				);
				return Err(failed(LOST_CONNECTION));
			}
			None => return Err(failed(LOST_CONNECTION)),
		}
	}
}

fn build_spec(state: &AppState, job: &Job, database: &Database) -> Result<DeploySpec, JobFailure> {
	let credentials = databases::decrypt_credentials(&state.secrets, database).map_err(|e| {
		tracing::error!(database = database.id.0, "{e}");
		failed("failed to read database credentials")
	})?;
	let engine = engines::spec(database.engine());
	let published_ports = database
		.public_port
		.map(|port| {
			u16::try_from(port)
				.map(|host| PublishedPort::tcp(host, engine.port))
				.map_err(|_| failed("database public_port is out of range"))
		})
		.transpose()?
		.into_iter()
		.collect();

	let service = format!("db-{}", database.id);
	Ok(DeploySpec {
		network_aliases: vec![format!("podkit-{service}")],
		service,
		instance_id: job.id.0,
		source: Source::Image {
			reference: engine.image(&database.version),
			// Picks up patch releases of the major tag.
			pull: true,
			auth: None,
		},
		env: engine.env(&credentials),
		command: engine
			.command
			.map(|c| c.iter().map(|part| (*part).to_owned()).collect()),
		container_port: None,
		routing: None,
		volumes: vec![VolumeMount {
			name: data_volume(database.id),
			path: engine.data_path(&database.version).to_owned(),
			read_only: false,
		}],
		published_ports,
		memory_bytes: database
			.memory_limit_mb
			.map(|mb| i64::from(mb) * 1024 * 1024),
		cpu_cores: database.cpu_limit.map(f64::from),
		strategy: Strategy::Recreate,
		gpus: None,
		entrypoint: None,
		shm_bytes: None,
		stop_timeout_s: None,
		networks: Vec::new(),
		options: proto::ContainerOptions::default(),
		ingress: Ingress {
			http_port: state.config.ingress_port,
			https_port: state.config.https_port,
			acme_email: state.config.acme_email.clone(),
		},
	})
}

fn data_volume(id: DatabaseId) -> String {
	format!("podkit-db-{id}-data")
}

async fn set_running(
	state: &AppState,
	log: &mut JobLog,
	database: &Database,
	client: &AgentClient,
	start: bool,
) -> Result<(), JobFailure> {
	let Some(container_id) = database.container_id.as_deref() else {
		return Err(failed("database has no container; use restart"));
	};
	let (action, progressive, status) = if start {
		("start", "starting", DatabaseStatus::Running)
	} else {
		("stop", "stopping", DatabaseStatus::Stopped)
	};
	log.push(
		Stream::System,
		&format!("{progressive} container {container_id}"),
	)
	.await;
	client
		.send(
			Method::POST,
			&format!("/v1/containers/{container_id}/{action}"),
		)
		.await
		.map_err(agent_failure)?;
	databases::set_status(&state.db, database.id, status, None)
		.await
		.map_err(|e| db_failure(&e))
}

/// Removes the containers and, when `purge`, the data volume, then the row.
async fn remove_database(
	state: &AppState,
	log: &mut JobLog,
	database: &Database,
	client: &AgentClient,
	purge: bool,
) -> Result<(), JobFailure> {
	log.push(Stream::System, "removing database containers")
		.await;
	client
		.send(Method::DELETE, &format!("/v1/services/db-{}", database.id))
		.await
		.map_err(agent_failure)?;
	if purge {
		log.push(Stream::System, "removing data volume").await;
		client
			.send(
				Method::DELETE,
				&format!("/v1/volumes/{}", data_volume(database.id)),
			)
			.await
			.map_err(agent_failure)?;
	}
	databases::delete(&state.db, database.id)
		.await
		.map_err(|e| db_failure(&e))
}
