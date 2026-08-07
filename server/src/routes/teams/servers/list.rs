use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::TeamId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

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

/// Lists servers under a team; caller must hold `read server` on that team.
pub async fn list_servers(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
) -> Result<Json<Vec<ServerResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"server",
	)
	.await?;

	let servers = state
		.servers
		.list_by_team(TeamId(team_id))
		.await?
		.into_iter()
		.map(|s| ServerResponse {
			id: s.id.0,
			team_id: s.team_id.0,
			name: s.name,
			hostname: s.hostname,
			ssh_port: s.ssh_port,
			ssh_user: s.ssh_user,
			podman_socket_path: s.podman_socket_path,
			is_local: s.is_local,
			status: s.status.as_str().to_string(),
		})
		.collect();

	Ok(Json(servers))
}
