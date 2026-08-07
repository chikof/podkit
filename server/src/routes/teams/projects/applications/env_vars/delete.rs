use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::EnvVarId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeleteEnvVarResponse {
	pub id: i64,
}

/// Deletes an env var; caller must hold `delete env_var` on the team. Must
/// belong to the application named in the path.
pub async fn delete_env_var(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, application_id, env_var_id)): Path<(i64, i64, i64, i64)>,
) -> Result<Json<DeleteEnvVarResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"delete",
		"env_var",
	)
	.await?;

	let env_var = state
		.env_vars
		.find_by_id(EnvVarId(env_var_id))
		.await?
		.filter(|e| e.application_id.0 == application_id)
		.ok_or(ServerError::EnvVarNotFound)?;

	state.env_vars.delete(env_var.id).await?;

	Ok(Json(DeleteEnvVarResponse { id: env_var.id.0 }))
}
