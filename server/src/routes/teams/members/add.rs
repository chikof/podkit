use axum::Json;
use axum::extract::{Path, State};
use crypto::generate_id;
use podkit_core::domain::shared::ids::{RoleId, TeamId, TeamMemberId};
use podkit_core::domain::team_member::TeamMember;
use serde::{Deserialize, Serialize};

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct AddMemberRequest {
	pub email: String,
	pub role_id: i64,
}

#[derive(Serialize)]
pub struct TeamMemberResponse {
	pub user_id: i64,
	pub role_id: i64,
}

pub async fn add_member(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path(team_id): Path<i64>,
	Json(body): Json<AddMemberRequest>,
) -> Result<Json<TeamMemberResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
		"membership",
	)
	.await?;

	let user = state
		.users
		.find_by_email(&body.email)
		.await?
		.ok_or_else(|| ServerError::Validation("no user with that email".to_string()))?;

	let role = state
		.roles
		.find_by_id(RoleId(body.role_id))
		.await?
		.ok_or(ServerError::RoleNotAllowed)?;
	if role.team_id.is_some_and(|t| t.0 != team_id) {
		return Err(ServerError::RoleNotAllowed);
	}

	if state
		.team_members
		.find_by_team_and_user(TeamId(team_id), user.id)
		.await?
		.is_some()
	{
		return Err(ServerError::AlreadyMember);
	}

	let member = TeamMember::new(
		TeamMemberId(generate_id()),
		TeamId(team_id),
		user.id,
		RoleId(body.role_id),
	);
	state.team_members.save(&member).await?;

	Ok(Json(TeamMemberResponse {
		user_id: member.user_id.0,
		role_id: member.role_id.0,
	}))
}
