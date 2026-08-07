use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::ServerId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeleteServerResponse {
	pub id: i64,
}

/// Deletes a remote server. The team's local server (`is_local`) cannot be
/// deleted, since that's the podkit host itself.
pub async fn delete_server(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, server_id)): Path<(i64, i64)>,
) -> Result<Json<DeleteServerResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"delete",
		"server",
	)
	.await?;

	let server = state
		.servers
		.find_by_id(ServerId(server_id))
		.await?
		.filter(|s| s.team_id.0 == team_id)
		.ok_or(ServerError::ServerNotFound)?;

	if server.is_local {
		return Err(ServerError::CannotDeleteLocalServer);
	}

	state.servers.delete(server.id).await?;

	Ok(Json(DeleteServerResponse { id: server.id.0 }))
}
