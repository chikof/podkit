use axum::Json;
use axum::extract::State;
use database::models::team::TeamModel;
use runtime::local_socket_path;
use serde::{Deserialize, Serialize};

use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct CreateTeamRequest {
	pub name: String,
	pub slug: String,
	#[serde(default)]
	pub logo: String,
}

#[derive(Serialize)]
pub struct TeamResponse {
	pub id: i64,
	pub name: String,
	pub slug: String,
	pub logo: String,
}

impl From<TeamModel> for TeamResponse {
	fn from(team: TeamModel) -> Self {
		Self {
			id: team.id,
			name: team.name,
			slug: team.slug,
			logo: team.logo,
		}
	}
}

/// Creates a team; the caller is unconditionally assigned the global `Owner` role.
pub async fn create_team(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Json(body): Json<CreateTeamRequest>,
) -> Result<Json<TeamResponse>, ServerError> {
	let team = TeamModel::create_with_owner(
		state.pool,
		body.name,
		body.slug,
		body.logo,
		claims.sub,
		&local_socket_path(),
	)
	.await?;

	Ok(Json(team.into()))
}
