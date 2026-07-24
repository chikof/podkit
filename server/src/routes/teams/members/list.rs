use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::TeamId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct TeamMemberResponse {
	pub user_id: i64,
	pub role_id: i64,
	pub joined_at: String,
}

pub async fn list_members(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
) -> Result<Json<Vec<TeamMemberResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"membership",
	)
	.await?;

	let members = state.team_members.list_by_team(TeamId(team_id)).await?;

	Ok(Json(
		members
			.into_iter()
			.map(|m| TeamMemberResponse {
				user_id: m.user_id.0,
				role_id: m.role_id.0,
				joined_at: m.joined_at.to_string(),
			})
			.collect(),
	))
}
