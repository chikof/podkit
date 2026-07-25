use axum::Json;
use axum::extract::{Path, State};
use crypto::generate_id;
use podkit_core::domain::project::Project;
use podkit_core::domain::shared::ids::{ProjectId, TeamId};
use serde::{Deserialize, Serialize};

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct CreateProjectRequest {
	pub name: String,
	pub slug: String,
}

#[derive(Serialize)]
pub struct ProjectResponse {
	pub id: i64,
	pub team_id: i64,
	pub name: String,
	pub slug: String,
}

impl From<Project> for ProjectResponse {
	fn from(project: Project) -> Self {
		Self {
			id: project.id.0,
			team_id: project.team_id.0,
			name: project.name,
			slug: project.slug,
		}
	}
}

/// Creates a project under a team; caller must hold `create project` on that team.
pub async fn create_project(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
	Json(body): Json<CreateProjectRequest>,
) -> Result<Json<ProjectResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
		"project",
	)
	.await?;

	let project = Project::new(
		ProjectId(generate_id()),
		TeamId(team_id),
		body.name,
		body.slug,
	);
	state.projects.save(&project).await?;

	Ok(Json(project.into()))
}
