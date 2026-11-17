use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Server, ServerStatus, host_key_fingerprint};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, TeamId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::{scope, validate};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{server_id}", delete(remove))
		.route("/{server_id}/bootstrap", post(bootstrap))
}

fn default_ssh_port() -> i32 {
	22
}

// `%t` is a systemd specifier: `/run` in system units (root), `$XDG_RUNTIME_DIR` in user
// units. One default so the rendered unit points at the right podman socket in both modes.
const DEFAULT_PODMAN_SOCKET_PATH: &str = "%t/podman/podman.sock";

fn default_podman_socket_path() -> String {
	DEFAULT_PODMAN_SOCKET_PATH.to_string()
}

#[derive(Deserialize)]
struct CreateServerRequest {
	name: String,
	hostname: String,
	#[serde(default = "default_ssh_port")]
	ssh_port: i32,
	ssh_user: String,
	/// Plaintext on the wire (TLS-protected); encrypted at rest before storage.
	ssh_private_key: String,
	#[serde(default = "default_podman_socket_path")]
	podman_socket_path: String,
}

#[derive(Deserialize)]
struct BootstrapQuery {
	/// Forget the pinned host key and scan it again (the host was reinstalled).
	#[serde(default)]
	reset_host_key: bool,
}

#[derive(Serialize)]
struct ServerResponse {
	id: ServerId,
	team_id: TeamId,
	name: String,
	hostname: String,
	ssh_port: i32,
	ssh_user: Option<String>,
	podman_socket_path: String,
	is_local: bool,
	status: ServerStatus,
	status_message: Option<String>,
	agent_version: Option<String>,
	arch: Option<String>,
	host_key_fingerprint: Option<String>,
}

impl From<&Server> for ServerResponse {
	fn from(server: &Server) -> Self {
		Self {
			id: server.id,
			team_id: server.team_id,
			name: server.name.clone(),
			hostname: server.hostname.clone(),
			ssh_port: server.ssh_port,
			ssh_user: server.ssh_user.clone(),
			podman_socket_path: server.podman_socket_path.clone(),
			is_local: server.is_local,
			status: server.status,
			status_message: server.status_message.clone(),
			agent_version: server.agent_version.clone(),
			arch: server.arch.clone(),
			host_key_fingerprint: server.host_key.as_deref().and_then(host_key_fingerprint),
		}
	}
}

#[derive(Serialize)]
struct DeleteServerResponse {
	id: ServerId,
}

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

/// Registers a remote server for a team. The team's local server is
/// provisioned automatically on team creation and cannot be created here.
///
/// The server is stored `pending` and a bootstrap job installs the agent in
/// the background; poll the server list for the outcome.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateServerRequest>,
) -> AppResult<(StatusCode, Json<ServerResponse>)> {
	authorize(&s.db, user.id, team, Action::Create, Resource::Server).await?;

	if !validate::server_host(&body.hostname) {
		return Err(ServerError::Validation("invalid hostname".to_string()));
	}
	if !validate::ssh_user(&body.ssh_user) {
		return Err(ServerError::Validation("invalid ssh_user".to_string()));
	}
	if !(validate::abs_path(&body.podman_socket_path)
		|| body.podman_socket_path == DEFAULT_PODMAN_SOCKET_PATH)
	{
		return Err(ServerError::Validation(
			"invalid podman_socket_path".to_string(),
		));
	}

	let encrypted_key = s
		.secrets
		.encrypt(&body.ssh_private_key)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt ssh key: {e}")))?;

	let server = Server::new_remote(
		team,
		body.name,
		body.hostname,
		body.ssh_port,
		body.ssh_user,
		encrypted_key,
		body.podman_socket_path,
	);

	super::insert(&s.db, &server).await?;
	enqueue_bootstrap(&s, &server).await?;

	Ok((StatusCode::ACCEPTED, Json(ServerResponse::from(&server))))
}

/// Lists servers under a team; caller must hold `read server` on that team.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ServerResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Server).await?;

	let servers = super::list_by_team(&s.db, team).await?;
	Ok(Json(servers.iter().map(ServerResponse::from).collect()))
}

/// Re-runs the bootstrap job: reinstalls the agent and refreshes the unit
/// file. The local server's agent is run by the operator, never by podkit.
async fn bootstrap(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ServerId)>,
	Query(query): Query<BootstrapQuery>,
) -> AppResult<(StatusCode, Json<ServerResponse>)> {
	authorize(&s.db, user.id, team, Action::Update, Resource::Server).await?;
	let mut server = scope::server(&s.db, team, id).await?;

	if server.is_local {
		return Err(ServerError::Validation(
			"local server agent is managed by the operator".to_string(),
		));
	}

	if query.reset_host_key {
		super::set_host_key(&s.db, server.id, None).await?;
		server.host_key = None;
	}
	enqueue_bootstrap(&s, &server).await?;

	Ok((StatusCode::ACCEPTED, Json(ServerResponse::from(&server))))
}

/// Deletes a remote server. The team's local server (`is_local`) cannot be
/// deleted, since that's the podkit host itself.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<DeleteServerResponse>> {
	authorize(&s.db, user.id, team, Action::Delete, Resource::Server).await?;
	let server = scope::server(&s.db, team, id).await?;

	if server.is_local {
		return Err(ServerError::CannotDeleteLocalServer);
	}

	super::delete(&s.db, team, server.id).await?;
	s.agents.forget(server.id);

	Ok(Json(DeleteServerResponse { id: server.id }))
}
