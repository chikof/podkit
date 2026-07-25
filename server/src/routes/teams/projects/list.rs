use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::TeamId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct ProjectResponse {
	pub id: i64,
	pub team_id: i64,
	pub name: String,
	pub slug: String,
}

/// Lists projects under a team; caller must hold `read project` on that team.
pub async fn list_projects(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
) -> Result<Json<Vec<ProjectResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"project",
	)
	.await?;

	let projects = state
		.projects
		.list_by_team(TeamId(team_id))
		.await?
		.into_iter()
		.map(|p| ProjectResponse {
			id: p.id.0,
			team_id: p.team_id.0,
			name: p.name,
			slug: p.slug,
		})
		.collect();

	Ok(Json(projects))
}
