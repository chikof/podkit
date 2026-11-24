//! Membership use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database.

use std::collections::HashMap;

use sqlx::PgPool;

use super::TeamMember;
pub use super::{MemberDetail, Role};
use crate::audit::{self, Diff, Target};
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

/// Adds an existing user to the team by email; needs `create membership`, and the role may
/// not reach further than the caller's own.
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
	super::ensure_within(db, user, team, role).await?;

	if super::find(db, team, invitee.id).await?.is_some() {
		return Err(ServerError::AlreadyMember);
	}

	let added = super::insert(db, team, invitee.id, role).await?;
	audit::log(
		db,
		user,
		team,
		"member.add",
		Target::new("member", invitee.id),
		Diff::new().set("role_id", role.0),
	)
	.await;
	Ok(added)
}

/// Changes a member's role; needs `update membership`. The role must be
/// built-in or the team's own, and neither the member's current role nor the new one may
/// reach further than the caller's own. Open invites the member sent that their new role no
/// longer covers are dropped.
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
	if !super::role_within(db, user, team, member.role_id).await? {
		return Err(ServerError::Validation(
			"You can't change the role of someone who can do more than you.".to_owned(),
		));
	}
	super::ensure_within(db, user, team, role).await?;

	let dropped = super::set_role(db, team, member.user_id, role).await?;
	let mut diff = Diff::new().change("role_id", member.role_id.0, role.0);
	if dropped > 0 {
		diff = diff.set("invites_dropped", dropped);
	}
	audit::log(
		db,
		user,
		team,
		"member.update",
		Target::new("member", member.user_id),
		diff,
	)
	.await;
	Ok(())
}

/// Which members are locked out of signing in after too many wrong attempts, and for how many
/// more seconds; needs `read membership`. Members who are free are left out.
pub async fn locked(db: &PgPool, user: UserId, team: TeamId) -> AppResult<HashMap<UserId, u64>> {
	authorize(db, user, team, Action::Read, Resource::Membership).await?;
	let ids: Vec<UserId> = super::list_details(db, team)
		.await?
		.into_iter()
		.map(|m| m.user_id)
		.collect();
	Ok(crate::throttle::locked_users(db, &ids).await?)
}

/// Lets a locked member try again now; needs `update membership`. Only the account's own
/// counters are cleared: an address that sprayed too many guesses still waits.
pub async fn unlock(db: &PgPool, user: UserId, team: TeamId, member: UserId) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Membership).await?;
	let member = super::find(db, team, member)
		.await?
		.ok_or_else(|| ServerError::Validation("member not found".to_string()))?;
	crate::throttle::clear_account(db, member.user_id).await;
	audit::log(
		db,
		user,
		team,
		"member.unlock",
		Target::new("member", member.user_id),
		Diff::new(),
	)
	.await;
	Ok(())
}
