//! `/api/v1/teams/{team_id}/presets`: what a preset asks, and applying it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::service::{self, Applied, Request};
use crate::auth::AuthUser;
use crate::auth::tokens::Scope;
use crate::error::{AppResult, ServerError};
use crate::ids::TeamId;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
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
		done @ Applied::Done { .. } => Json(done).into_response(),
	})
}
