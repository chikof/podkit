use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::TeamId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct TeamMemberResponse {
	pub user_id: i64,
	pub email: String,
	pub display_name: String,
	pub role_id: i64,
	pub role_name: String,
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

	let mut responses = Vec::with_capacity(members.len());
	for m in members {
		let user = state.users.find_by_id(m.user_id).await?;
		let role = state.roles.find_by_id(m.role_id).await?;

		responses.push(TeamMemberResponse {
			user_id: m.user_id.0,
			email: user
				.as_ref()
				.map_or_else(String::new, |u| u.email.as_str().to_string()),
			display_name: user.map_or_else(String::new, |u| u.display_name),
			role_id: m.role_id.0,
			role_name: role.map_or_else(String::new, |r| r.name),
			joined_at: m.joined_at.to_string(),
		});
	}

	Ok(Json(responses))
}
