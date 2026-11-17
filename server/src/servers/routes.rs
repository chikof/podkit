use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Server, ServerStatus, probe};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, TeamId};
use crate::state::AppState;
use crate::{scope, validate};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{server_id}", delete(remove))
}

fn default_ssh_port() -> i32 {
	22
}

fn default_podman_socket_path() -> String {
	"/run/podman/podman.sock".to_string()
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
		}
	}
}

#[derive(Serialize)]
struct DeleteServerResponse {
	id: ServerId,
}

/// Registers a remote server for a team. The team's local server is
/// provisioned automatically on team creation and cannot be created here.
///
/// Probes connectivity over ssh and the podman API immediately: on
/// success the server is marked `active`, otherwise it's persisted
/// `pending` so the caller can fix access and retry later rather than
/// losing the registration outright.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateServerRequest>,
) -> AppResult<Json<ServerResponse>> {
	authorize(&s.db, user.id, team, Action::Create, Resource::Server).await?;

	if !validate::server_host(&body.hostname) {
		return Err(ServerError::Validation("invalid hostname".to_string()));
	}
	if !validate::ssh_user(&body.ssh_user) {
		return Err(ServerError::Validation("invalid ssh_user".to_string()));
	}
	if !validate::abs_path(&body.podman_socket_path) {
		return Err(ServerError::Validation(
			"invalid podman_socket_path".to_string(),
		));
	}

	let encrypted_key = s
		.secrets
		.encrypt(&body.ssh_private_key)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt ssh key: {e}")))?;

	let mut server = Server::new_remote(
		team,
		body.name,
		body.hostname,
		body.ssh_port,
		body.ssh_user,
		encrypted_key,
		body.podman_socket_path,
	);

	if probe(&server, &s.secrets).await {
		server.status = ServerStatus::Active;
	} else {
		tracing::warn!(server = %server.name, "server registered but connectivity probe failed, leaving pending");
	}

	super::insert(&s.db, &server).await?;

	Ok(Json(ServerResponse::from(&server)))
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

	Ok(Json(DeleteServerResponse { id: server.id }))
}
