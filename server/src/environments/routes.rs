use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Environment, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{EnvironmentId, ProjectId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list).post(create))
		.route("/order", put(reorder))
		.route("/{environment_id}", get(show).patch(update).delete(remove))
}

#[derive(Deserialize)]
struct CreateRequest {
	name: String,
	slug: String,
}

/// `slug` is optional: omitted keeps the current one.
#[derive(Deserialize)]
struct UpdateRequest {
	name: String,
	#[serde(default)]
	slug: Option<String>,
}

#[derive(Deserialize)]
struct OrderRequest {
	/// Every environment of the project, first = default.
	ids: Vec<EnvironmentId>,
}

#[derive(Serialize)]
struct EnvironmentResponse {
	id: EnvironmentId,
	project_id: ProjectId,
	name: String,
	slug: String,
	position: i32,
	created_at: String,
}

impl From<&Environment> for EnvironmentResponse {
	fn from(e: &Environment) -> Self {
		Self {
			id: e.id,
			project_id: e.project_id,
			name: e.name.clone(),
			slug: e.slug.clone(),
			position: e.position,
			created_at: e.created_at.to_string(),
		}
	}
}

#[derive(Serialize)]
struct DeleteResponse {
	id: EnvironmentId,
}

/// Adds an environment to a project; caller must hold `create project`.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<CreateRequest>,
) -> AppResult<Json<EnvironmentResponse>> {
	let env = service::create(&s.db, user.id, team, project, &body.name, &body.slug).await?;
	Ok(Json(EnvironmentResponse::from(&env)))
}

/// Lists a project's environments in display order; caller must hold
/// `read project`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<Vec<EnvironmentResponse>>> {
	let envs = service::list(&s.db, user.id, team, project).await?;
	Ok(Json(envs.iter().map(EnvironmentResponse::from).collect()))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, EnvironmentId)>,
) -> AppResult<Json<EnvironmentResponse>> {
	let env = service::get(&s.db, user.id, team, project, id).await?;
	Ok(Json(EnvironmentResponse::from(&env)))
}

/// Renames an environment; caller must hold `update project`.
async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, EnvironmentId)>,
	Json(body): Json<UpdateRequest>,
) -> AppResult<Json<EnvironmentResponse>> {
	let env = service::rename(
		&s.db,
		user.id,
		team,
		project,
		id,
		&body.name,
		body.slug.as_deref(),
	)
	.await?;
	Ok(Json(EnvironmentResponse::from(&env)))
}

/// Reorders the project's environments; caller must hold `update project`.
/// The first one is the default for resource create and list calls.
async fn reorder(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<OrderRequest>,
) -> AppResult<Json<Vec<EnvironmentResponse>>> {
	let envs = service::reorder(&s.db, user.id, team, project, &body.ids).await?;
	Ok(Json(envs.iter().map(EnvironmentResponse::from).collect()))
}

/// Deletes an empty environment that isn't the project's last; caller must
/// hold `delete project`. A 409 names what still lives in it.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, EnvironmentId)>,
) -> AppResult<Json<DeleteResponse>> {
	let env = service::remove(&s.db, user.id, team, project, id).await?;
	Ok(Json(DeleteResponse { id: env.id }))
}
