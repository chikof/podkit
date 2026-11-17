//! Server use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or the job queue.

use sqlx::PgPool;

use super::Server;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, TeamId, UserId};
use crate::jobs::{self, Job, JobKind};
use crate::state::AppState;
use crate::{scope, validate};

// `%t` is a systemd specifier: `/run` in system units (root), `$XDG_RUNTIME_DIR` in user
// units. One default so the rendered unit points at the right podman socket in both modes.
pub const DEFAULT_PODMAN_SOCKET_PATH: &str = "%t/podman/podman.sock";

pub const DEFAULT_SSH_PORT: i32 = 22;

const MAX_NAME_LEN: usize = 60;

/// What it takes to register a remote server. The key is plaintext here and
/// encrypted before it is stored; nothing reads it back out.
pub struct NewServer {
	pub name: String,
	pub hostname: String,
	pub ssh_port: i32,
	pub ssh_user: String,
	pub ssh_private_key: String,
	pub podman_socket_path: String,
}

/// A server and its newest bootstrap job, for the detail page.
#[derive(Debug, Clone)]
pub struct ServerDetail {
	pub server: Server,
	pub bootstrap: Option<Job>,
}

/// Queues the job that installs (or refreshes) the agent on `server`.
async fn enqueue_bootstrap(s: &AppState, server: &Server) -> AppResult<()> {
	jobs::enqueue(
		s,
		JobKind::ServerBootstrap,
		server.id,
		server.id.0,
		format!("server:{}", server.id),
	)
	.await?;
	Ok(())
}

/// Registers a remote server and queues its bootstrap; needs `create server`.
/// The team's local server is provisioned with the team and cannot be created
/// here. The server is stored `pending` until the bootstrap job finishes.
pub async fn create(s: &AppState, user: UserId, team: TeamId, new: NewServer) -> AppResult<Server> {
	authorize(&s.db, user, team, Action::Create, Resource::Server).await?;

	let name = new.name.trim().to_owned();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	if !validate::server_host(&new.hostname) {
		return Err(ServerError::Validation("invalid hostname".to_string()));
	}
	if !(1..=65535).contains(&new.ssh_port) {
		return Err(ServerError::Validation(
			"SSH port must be between 1 and 65535.".to_string(),
		));
	}
	if !validate::ssh_user(&new.ssh_user) {
		return Err(ServerError::Validation("invalid ssh_user".to_string()));
	}
	if !(validate::abs_path(&new.podman_socket_path)
		|| new.podman_socket_path == DEFAULT_PODMAN_SOCKET_PATH)
	{
		return Err(ServerError::Validation(
			"invalid podman_socket_path".to_string(),
		));
	}
	if new.ssh_private_key.trim().is_empty() {
		return Err(ServerError::Validation(
			"An SSH private key is required.".to_string(),
		));
	}

	let encrypted_key = s
		.secrets
		.encrypt(&new.ssh_private_key)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt ssh key: {e}")))?;

	let server = Server::new_remote(
		team,
		name,
		new.hostname,
		new.ssh_port,
		new.ssh_user,
		encrypted_key,
		new.podman_socket_path,
	);

	super::insert(&s.db, &server).await?;
	enqueue_bootstrap(s, &server).await?;
	Ok(server)
}

/// Lists a team's servers; needs `read server`.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Server>> {
	authorize(db, user, team, Action::Read, Resource::Server).await?;
	Ok(super::list_by_team(db, team).await?)
}

/// One server and its newest bootstrap job; needs `read server`. A server of
/// another team is `ServerNotFound`.
pub async fn get(db: &PgPool, user: UserId, team: TeamId, id: ServerId) -> AppResult<ServerDetail> {
	authorize(db, user, team, Action::Read, Resource::Server).await?;
	let server = scope::server(db, team, id).await?;
	let bootstrap = jobs::latest_for(db, JobKind::ServerBootstrap, server.id.0).await?;
	Ok(ServerDetail { server, bootstrap })
}

/// Re-runs the bootstrap job: reinstalls the agent and refreshes the unit
/// file. With `reset_host_key` the pinned host key is forgotten first and
/// scanned again. Needs `update server`; the local server's agent is run by
/// the operator, never by podkit.
pub async fn bootstrap(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: ServerId,
	reset_host_key: bool,
) -> AppResult<Server> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let mut server = scope::server(&s.db, team, id).await?;

	if server.is_local {
		return Err(ServerError::Validation(
			"local server agent is managed by the operator".to_string(),
		));
	}

	if reset_host_key {
		super::set_host_key(&s.db, server.id, None).await?;
		server.host_key = None;
	}
	enqueue_bootstrap(s, &server).await?;
	Ok(server)
}

/// Deletes a remote server; needs `delete server`. The team's local server
/// cannot be deleted, since that is the podkit host itself, and a server that
/// still runs applications or databases is refused.
pub async fn remove(s: &AppState, user: UserId, team: TeamId, id: ServerId) -> AppResult<Server> {
	authorize(&s.db, user, team, Action::Delete, Resource::Server).await?;
	let server = scope::server(&s.db, team, id).await?;

	if server.is_local {
		return Err(ServerError::CannotDeleteLocalServer);
	}

	super::delete(&s.db, team, server.id).await.map_err(|e| {
		if crate::db::is_reference_violation(&e) {
			ServerError::Conflict(
				"This server still runs applications or databases. Remove them first.".to_owned(),
			)
		} else {
			e.into()
		}
	})?;
	s.agents.forget(server.id);
	Ok(server)
}
