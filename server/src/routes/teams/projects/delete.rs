use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::ProjectId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeleteProjectResponse {
	pub id: i64,
}

/// Deletes a project; caller must hold `delete project` on the owning team.
/// The project must belong to the team named in the path, otherwise 404,
/// same as if it never existed, to avoid leaking cross-team project ids.
pub async fn delete_project(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id)): Path<(i64, i64)>,
) -> Result<Json<DeleteProjectResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"delete",
		"project",
	)
	.await?;

	let project = state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	state.projects.delete(project.id).await?;

	Ok(Json(DeleteProjectResponse { id: project.id.0 }))
}
