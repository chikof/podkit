use axum::routing::post;
use axum::{Json, Router, extract::State};
use serde::{Deserialize, Serialize};

use super::{Team, list_for_user, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::TeamId;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route("/", post(create).get(list))
}

#[derive(Deserialize)]
struct CreateTeamRequest {
	name: String,
	slug: String,
	#[serde(default)]
	logo: String,
}

#[derive(Serialize)]
struct TeamResponse {
	id: TeamId,
	name: String,
	slug: String,
	logo: String,
}

impl From<&Team> for TeamResponse {
	fn from(team: &Team) -> Self {
		Self {
			id: team.id,
			name: team.name.clone(),
			slug: team.slug.clone(),
			logo: team.logo.clone(),
		}
	}
}

/// Creates a team; the caller is unconditionally assigned the global `Owner` role.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Json(body): Json<CreateTeamRequest>,
) -> AppResult<Json<TeamResponse>> {
	let team = service::create(&s.db, user.id, &body.name, &body.slug, &body.logo).await?;
	Ok(Json(TeamResponse::from(&team)))
}

/// Lists every team the caller is a member of.
async fn list(State(s): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<TeamResponse>>> {
	let teams = list_for_user(&s.db, user.id).await?;
	Ok(Json(teams.iter().map(TeamResponse::from).collect()))
}
