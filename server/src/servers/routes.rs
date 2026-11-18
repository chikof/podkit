use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, DEFAULT_PODMAN_SOCKET_PATH, DEFAULT_SSH_PORT, NewServer};
use super::{Server, ServerStatus, host_key_fingerprint};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ServerId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{server_id}", delete(remove))
		.route("/{server_id}/bootstrap", post(bootstrap))
}

fn default_ssh_port() -> i32 {
	DEFAULT_SSH_PORT
}

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
	/// Latest snapshot from the agent; `None` until the first fetch succeeds.
	hardware: Option<proto::Hardware>,
	/// RFC 3339 time of `hardware`.
	hardware_at: Option<String>,
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
			hardware: server.hardware.as_ref().map(|h| h.0.clone()),
			hardware_at: server.hardware_at.and_then(|t| {
				t.format(&time::format_description::well_known::Rfc3339)
					.ok()
			}),
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
/// The server is stored `pending` and a bootstrap job installs the agent in
/// the background; poll the server list for the outcome.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateServerRequest>,
) -> AppResult<(StatusCode, Json<ServerResponse>)> {
	let server = service::create(
		&s,
		user.id,
		team,
		NewServer {
			name: body.name,
			hostname: body.hostname,
			ssh_port: body.ssh_port,
			ssh_user: body.ssh_user,
			ssh_private_key: body.ssh_private_key,
			podman_socket_path: body.podman_socket_path,
		},
	)
	.await?;
	Ok((StatusCode::ACCEPTED, Json(ServerResponse::from(&server))))
}

/// Lists servers under a team; caller must hold `read server` on that team.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ServerResponse>>> {
	let servers = service::list(&s.db, user.id, team).await?;
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
	let server = service::bootstrap(&s, user.id, team, id, query.reset_host_key).await?;
	Ok((StatusCode::ACCEPTED, Json(ServerResponse::from(&server))))
}

/// Deletes a remote server. The team's local server (`is_local`) cannot be
/// deleted, since that's the podkit host itself.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<DeleteServerResponse>> {
	let server = service::remove(&s, user.id, team, id).await?;
	Ok(Json(DeleteServerResponse { id: server.id }))
}
