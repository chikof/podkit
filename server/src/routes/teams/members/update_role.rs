use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use podkit_core::domain::shared::ids::{RoleId, TeamId, UserId};
use serde::Deserialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct UpdateRoleRequest {
	pub role_id: i64,
}

pub async fn update_member_role(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, user_id)): Path<(i64, i64)>,
	Json(body): Json<UpdateRoleRequest>,
) -> Result<StatusCode, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"update",
		"membership",
	)
	.await?;

	let mut member = state
		.team_members
		.find_by_team_and_user(TeamId(team_id), UserId(user_id))
		.await?
		.ok_or_else(|| ServerError::Validation("member not found".to_string()))?;

	let role = state
		.roles
		.find_by_id(RoleId(body.role_id))
		.await?
		.ok_or(ServerError::RoleNotAllowed)?;
	if role.team_id.is_some_and(|t| t.0 != team_id) {
		return Err(ServerError::RoleNotAllowed);
	}

	member.change_role(RoleId(body.role_id));
	state.team_members.save(&member).await?;

	Ok(StatusCode::NO_CONTENT)
}
