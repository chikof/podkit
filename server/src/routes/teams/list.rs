use axum::Json;
use axum::extract::State;
use database::models::team::TeamModel;
use serde::Serialize;

use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct TeamsResponse {
	pub id: i64,
	pub name: String,
	pub slug: String,
	pub logo: String,
}

impl From<&TeamModel> for TeamsResponse {
	fn from(team: &TeamModel) -> Self {
		Self {
			id: team.id,
			name: team.name.clone(),
			slug: team.slug.clone(),
			logo: team.logo.clone(),
		}
	}
}

/// Creates a team; the caller is unconditionally assigned the global `Owner` role.
pub async fn list_teams(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
) -> Result<Json<Vec<TeamsResponse>>, ServerError> {
	let team = TeamModel::list(state.pool, claims.sub)
		.await?
		.iter()
		.map(std::convert::Into::into)
		.collect();

	Ok(Json(team))
}
