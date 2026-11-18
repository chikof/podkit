//! Membership use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database.

use sqlx::PgPool;

use super::TeamMember;
pub use super::{MemberDetail, Role};
use crate::auth::permission::{Action, Resource, authorize};
use crate::auth::{normalize_email_lenient, users};
use crate::error::{AppResult, ServerError};
use crate::ids::{RoleId, TeamId, UserId};

/// The team's members with their roles; needs `read membership`.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<MemberDetail>> {
	authorize(db, user, team, Action::Read, Resource::Membership).await?;
	Ok(super::list_details(db, team).await?)
}

/// The roles a team can assign: built-in ones plus its own; needs `read role`.
pub async fn roles(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Role>> {
	authorize(db, user, team, Action::Read, Resource::Role).await?;
	Ok(super::list_roles(db, team).await?)
}

/// Adds an existing user to the team by email; needs `create membership`.
pub async fn add(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	email: &str,
	role: RoleId,
) -> AppResult<TeamMember> {
	authorize(db, user, team, Action::Create, Resource::Membership).await?;

	let invitee = users::find_by_email(db, &normalize_email_lenient(email))
		.await?
		.ok_or_else(|| ServerError::Validation("no user with that email".to_string()))?;

	super::ensure_assignable(db, team, role).await?;

	if super::find(db, team, invitee.id).await?.is_some() {
		return Err(ServerError::AlreadyMember);
	}

	Ok(super::insert(db, team, invitee.id, role).await?)
}

/// Changes a member's role; needs `update membership`. The role must be
/// built-in or the team's own.
pub async fn update_role(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	member: UserId,
	role: RoleId,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Membership).await?;

	let member = super::find(db, team, member)
		.await?
		.ok_or_else(|| ServerError::Validation("member not found".to_string()))?;

	super::ensure_assignable(db, team, role).await?;

	super::set_role(db, team, member.user_id, role).await?;
	Ok(())
}
