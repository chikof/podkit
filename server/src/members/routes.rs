use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::invites::{self, Invite, Issued};
use super::{MemberDetail, Role, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{RoleId, TeamId, TeamInviteId, UserId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/members", get(list).post(add))
		.route("/members/{user_id}", patch(update_role))
		.route("/members/{user_id}/unlock", post(unlock))
		.route("/roles", get(list_roles))
		.route("/invites", get(list_invites).post(invite))
		.route("/invites/{invite_id}", delete(revoke_invite))
		.route("/invites/{invite_id}/resend", post(resend_invite))
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
	/// Where the member's picture is served, if they have one.
	avatar_url: Option<String>,
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
			avatar_url: m
				.avatar
				.as_deref()
				.map(|h| crate::avatars::user_url(m.user_id, h)),
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

/// Clears a member's sign-in lockout. Caller must hold `update membership`.
async fn unlock(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, member_user)): Path<(TeamId, UserId)>,
) -> AppResult<StatusCode> {
	service::unlock(&s.db, user.id, team, member_user).await?;
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct InviteRequest {
	email: String,
	role_id: RoleId,
}

#[derive(Serialize)]
struct InviteResponse {
	id: TeamInviteId,
	email: String,
	role_id: RoleId,
	role_name: String,
	invited_by: Option<String>,
	created_at: String,
	expires_at: String,
	expired: bool,
}

impl From<&Invite> for InviteResponse {
	fn from(i: &Invite) -> Self {
		Self {
			id: i.id,
			email: i.email.clone(),
			role_id: i.role_id,
			role_name: i.role_name.clone(),
			invited_by: i.invited_by.clone(),
			created_at: i.created_at.to_string(),
			expires_at: i.expires_at.to_string(),
			expired: i.expired(time::OffsetDateTime::now_utc()),
		}
	}
}

/// A new or resent invite. `link` is there only when the email was not sent: the caller
/// passes it on, and it is never shown again.
#[derive(Serialize)]
struct IssuedResponse {
	#[serde(flatten)]
	invite: InviteResponse,
	emailed: bool,
	link: Option<String>,
	note: Option<String>,
}

impl From<Issued> for IssuedResponse {
	fn from(i: Issued) -> Self {
		Self {
			invite: InviteResponse::from(&i.invite),
			emailed: i.emailed,
			link: i.link,
			note: i.note,
		}
	}
}

/// Open invites; caller must hold `read membership`.
async fn list_invites(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<InviteResponse>>> {
	let invites = invites::list(&s.db, user.id, team).await?;
	Ok(Json(invites.iter().map(InviteResponse::from).collect()))
}

/// Invites an email address with a role; caller must hold `create membership`.
async fn invite(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<InviteRequest>,
) -> AppResult<(StatusCode, Json<IssuedResponse>)> {
	let issued = invites::create(&s, user.id, team, &body.email, body.role_id).await?;
	Ok((StatusCode::CREATED, Json(issued.into())))
}

/// A new link for an invite, sent again; caller must hold `create membership`.
async fn resend_invite(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, TeamInviteId)>,
) -> AppResult<Json<IssuedResponse>> {
	let issued = invites::resend(&s, user.id, team, id).await?;
	Ok(Json(issued.into()))
}

/// Withdraws an invite; caller must hold `delete membership`.
async fn revoke_invite(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, TeamInviteId)>,
) -> AppResult<StatusCode> {
	invites::revoke(&s.db, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}
