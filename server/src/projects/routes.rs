use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::Project;
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::AppResult;
use crate::ids::{ProjectId, TeamId};
use crate::scope;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{project_id}", delete(remove))
}

#[derive(Deserialize)]
struct CreateProjectRequest {
	name: String,
	slug: String,
}

#[derive(Serialize)]
struct ProjectResponse {
	id: ProjectId,
	team_id: TeamId,
	name: String,
	slug: String,
}

impl From<&Project> for ProjectResponse {
	fn from(project: &Project) -> Self {
		Self {
			id: project.id,
			team_id: project.team_id,
			name: project.name.clone(),
			slug: project.slug.clone(),
		}
	}
}

#[derive(Serialize)]
struct DeleteProjectResponse {
	id: ProjectId,
}

/// Creates a project under a team; caller must hold `create project` on that team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateProjectRequest>,
) -> AppResult<Json<ProjectResponse>> {
	authorize(&s.db, user.id, team, Action::Create, Resource::Project).await?;

	let project = super::insert(&s.db, team, &body.name, &body.slug).await?;
	Ok(Json(ProjectResponse::from(&project)))
}

/// Lists projects under a team; caller must hold `read project` on that team.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ProjectResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Project).await?;

	let projects = super::list_by_team(&s.db, team).await?;
	Ok(Json(projects.iter().map(ProjectResponse::from).collect()))
}

/// Deletes a project; caller must hold `delete project` on the owning team.
/// A project of another team is a 404, same as if it never existed.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<DeleteProjectResponse>> {
	authorize(&s.db, user.id, team, Action::Delete, Resource::Project).await?;
	let project = scope::project(&s.db, team, id).await?;

	super::delete(&s.db, team, project.id).await?;

	Ok(Json(DeleteProjectResponse { id: project.id }))
}
