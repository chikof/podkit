use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{MemberDetail, Role, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{RoleId, TeamId, UserId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/members", get(list).post(add))
		.route("/members/{user_id}", patch(update_role))
		.route("/roles", get(list_roles))
}

#[derive(Deserialize)]
struct AddMemberRequest {
	email: String,
	role_id: RoleId,
}

#[derive(Serialize)]
struct AddedMemberResponse {
	user_id: UserId,
	role_id: RoleId,
}

#[derive(Serialize)]
struct MemberResponse {
	user_id: UserId,
	email: String,
	display_name: String,
	role_id: RoleId,
	role_name: String,
	joined_at: String,
}

impl From<&MemberDetail> for MemberResponse {
	fn from(m: &MemberDetail) -> Self {
		Self {
			user_id: m.user_id,
			email: m.email.clone(),
			display_name: m.display_name.clone(),
			role_id: m.role_id,
			role_name: m.role_name.clone(),
			joined_at: m.joined_at.to_string(),
		}
	}
}

#[derive(Deserialize)]
struct UpdateRoleRequest {
	role_id: RoleId,
}

#[derive(Serialize)]
struct RoleResponse {
	id: RoleId,
	name: String,
	/// `true` for the built-in roles every team gets (seeded by
	/// `0009_seed_rbac_defaults`), shared across all teams rather than
	/// owned by this one.
	is_builtin: bool,
}

impl From<&Role> for RoleResponse {
	fn from(r: &Role) -> Self {
		Self {
			id: r.id,
			name: r.name.clone(),
			is_builtin: r.is_builtin(),
		}
	}
}

async fn add(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<AddMemberRequest>,
) -> AppResult<Json<AddedMemberResponse>> {
	let member = service::add(&s.db, user.id, team, &body.email, body.role_id).await?;

	Ok(Json(AddedMemberResponse {
		user_id: member.user_id,
		role_id: member.role_id,
	}))
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<MemberResponse>>> {
	let members = service::list(&s.db, user.id, team).await?;
	Ok(Json(members.iter().map(MemberResponse::from).collect()))
}

async fn update_role(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, member_user)): Path<(TeamId, UserId)>,
	Json(body): Json<UpdateRoleRequest>,
) -> AppResult<StatusCode> {
	service::update_role(&s.db, user.id, team, member_user, body.role_id).await?;
	Ok(StatusCode::NO_CONTENT)
}

/// Lists the roles assignable within a team: the built-in roles plus any
/// this team has defined. Lets the dashboard populate a role picker for
/// `POST/PATCH .../members`. Caller must hold `read role` on the team.
async fn list_roles(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<RoleResponse>>> {
	let roles = service::roles(&s.db, user.id, team).await?;
	Ok(Json(roles.iter().map(RoleResponse::from).collect()))
}
