//! `/api/v1/teams/{team_id}/presets`: what a preset asks, and applying it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::service::{self, Applied, NewPreset, Request};
use super::store;
use crate::auth::AuthUser;
use crate::auth::tokens::Scope;
use crate::error::{AppResult, ServerError};
use crate::ids::{PresetId, ProjectId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list).post(import))
		.route("/{preset_id}", get(one).delete(remove))
		.route(
			"/from-project/{project_id}",
			get(from_project).post(save_project),
		)
		.route("/needs", post(needs))
		.route("/plan", post(plan))
		.route("/apply", post(apply))
}

#[derive(Deserialize)]
struct Preset {
	preset: String,
}

/// What the preset asks, and the team's servers, keys and registries to answer with.
async fn needs(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<Preset>,
) -> AppResult<Json<service::Asked>> {
	Ok(Json(service::asked(&s, user.id, team, &body.preset).await?))
}

/// What applying it with these answers would do. Nothing changes.
async fn plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(request): Json<Request>,
) -> AppResult<Response> {
	let plan = service::plan(&s, user.id, team, &request).await?;
	Ok(Json(plan).into_response())
}

/// Applies it; 422 with the plan when the plan has issues, and nothing changed.
async fn apply(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(request): Json<Request>,
) -> AppResult<Response> {
	if request.deploy
		&& let Some(grant) = &user.grant
		&& !grant.allows(Scope::Deploy)
	{
		return Err(ServerError::MissingScope("deploy"));
	}
	Ok(match service::apply(&s, user.id, team, &request).await? {
		Applied::Refused(plan) => (
			StatusCode::UNPROCESSABLE_ENTITY,
			Json(json!({ "error": "the plan has issues; nothing was changed", "plan": plan })),
		)
			.into_response(),
		Applied::Failed {
			project_id,
			applied,
		} => (
			StatusCode::CONFLICT,
			Json(json!({
				"error": "a change failed; what was made before it remains in the project",
				"project_id": project_id,
				"applied": applied,
			})),
		)
			.into_response(),
		done @ Applied::Done { .. } => Json(done).into_response(),
	})
}

/// The team's kept presets, without their files.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<store::Preset>>> {
	Ok(Json(service::list(&s, user.id, team).await?))
}

async fn one(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, PresetId)>,
) -> AppResult<Json<store::Preset>> {
	Ok(Json(service::get(&s, user.id, team, id).await?))
}

#[derive(Deserialize)]
struct Import {
	#[serde(flatten)]
	new: NewPreset,
	preset: String,
}

/// Keeps a preset handed in as a file; 201. It is marked `imported`.
async fn import(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<Import>,
) -> AppResult<(StatusCode, Json<store::Preset>)> {
	let kept = service::import(&s, user.id, team, &body.new, body.preset).await?;
	Ok((StatusCode::CREATED, Json(kept)))
}

/// What saving the project as a preset would keep, to be read first. Saves nothing.
async fn from_project(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<serde_json::Value>> {
	let preset = service::from_project(&s, user.id, team, project).await?;
	Ok(Json(json!({ "preset": preset })))
}

/// Saves the project as a preset of the team; 201.
async fn save_project(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(new): Json<NewPreset>,
) -> AppResult<(StatusCode, Json<store::Preset>)> {
	let kept = service::save_project(&s, user.id, team, project, &new).await?;
	Ok((StatusCode::CREATED, Json(kept)))
}

async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, PresetId)>,
) -> AppResult<StatusCode> {
	service::remove(&s, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}
