use axum::Json;
use axum::extract::{Path, State};
use crypto::generate_id;
use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::server::{Server, ServerStatus};
use podkit_core::domain::shared::ids::{ServerId, TeamId};
use runtime::ServerConnection;
use serde::{Deserialize, Serialize};

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

fn default_ssh_port() -> i32 {
	22
}

fn default_podman_socket_path() -> String {
	"/run/podman/podman.sock".to_string()
}

#[derive(Deserialize)]
pub struct CreateServerRequest {
	pub name: String,
	pub hostname: String,
	#[serde(default = "default_ssh_port")]
	pub ssh_port: i32,
	pub ssh_user: String,
	/// Plaintext on the wire (TLS-protected); encrypted at rest before storage.
	pub ssh_private_key: String,
	#[serde(default = "default_podman_socket_path")]
	pub podman_socket_path: String,
}

#[derive(Serialize)]
pub struct ServerResponse {
	pub id: i64,
	pub team_id: i64,
	pub name: String,
	pub hostname: String,
	pub ssh_port: i32,
	pub ssh_user: Option<String>,
	pub podman_socket_path: String,
	pub is_local: bool,
	pub status: String,
}

impl From<Server> for ServerResponse {
	fn from(server: Server) -> Self {
		Self {
			id: server.id.0,
			team_id: server.team_id.0,
			name: server.name,
			hostname: server.hostname,
			ssh_port: server.ssh_port,
			ssh_user: server.ssh_user,
			podman_socket_path: server.podman_socket_path,
			is_local: server.is_local,
			status: server.status.as_str().to_string(),
		}
	}
}

/// Registers a remote server for a team. The team's local server is
/// provisioned automatically on team creation and cannot be created here.
///
/// Probes connectivity over ssh and the podman API immediately: on
/// success the server is marked `active`, otherwise it's persisted
/// `pending` so the caller can fix access and retry later rather than
/// losing the registration outright.
pub async fn create_server(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
	Json(body): Json<CreateServerRequest>,
) -> Result<Json<ServerResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
		"server",
	)
	.await?;

	let encrypted_key = state
		.secrets
		.encrypt(&body.ssh_private_key)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt ssh key: {e}")))?;

	let mut server = Server::new_remote(
		ServerId(generate_id()),
		TeamId(team_id),
		body.name,
		body.hostname,
		body.ssh_port,
		body.ssh_user,
		encrypted_key,
		body.podman_socket_path,
	);

	match ServerConnection::connect(&server, &state.secrets).await {
		Ok(connection) if connection.runtime.ping().await.is_ok() => {
			server.status = ServerStatus::Active;
		}
		Ok(_) | Err(_) => {
			tracing::warn!(server = %server.name, "server registered but connectivity probe failed, leaving pending");
		}
	}

	state.servers.save(&server).await?;

	Ok(Json(server.into()))
}
