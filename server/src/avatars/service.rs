//! Avatar use cases shared by the JSON API and the web UI: authorize, then validate, then
//! touch the database.

use sqlx::PgPool;

use super::Stored;
use super::process::process;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};

/// Sets the team's picture; needs `update team`. Returns the new image's hash.
///
/// # Errors
/// `Forbidden` without the permission; see [`process`] for the rejected uploads.
pub async fn set_team(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	declared: Option<&str>,
	data: &[u8],
) -> AppResult<String> {
	authorize(db, user, team, Action::Update, Resource::Team).await?;
	let image = process(declared, data)?;
	super::put_team(db, team, &image).await?;
	Ok(image.hash)
}

/// Removes the team's picture, so the monogram shows again; needs `update team`.
pub async fn clear_team(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Team).await?;
	Ok(super::delete_team(db, team).await?)
}

/// Sets `owner`'s picture. Only the owner may: `actor` must be `owner`.
///
/// # Errors
/// `Forbidden` when `actor` is someone else; see [`process`] for the rejected uploads.
pub async fn set_user(
	db: &PgPool,
	actor: UserId,
	owner: UserId,
	declared: Option<&str>,
	data: &[u8],
) -> AppResult<String> {
	if actor != owner {
		return Err(ServerError::Forbidden);
	}
	let image = process(declared, data)?;
	super::put_user(db, owner, &image).await?;
	Ok(image.hash)
}

/// Removes `owner`'s picture. Only the owner may.
pub async fn clear_user(db: &PgPool, actor: UserId, owner: UserId) -> AppResult<()> {
	if actor != owner {
		return Err(ServerError::Forbidden);
	}
	Ok(super::delete_user(db, owner).await?)
}

/// The team's picture for `viewer`. A team the viewer is not in is a 404, the same as a
/// team with no picture, so the answer never says which teams exist.
pub async fn team_image(db: &PgPool, viewer: UserId, team: TeamId) -> AppResult<Stored> {
	if crate::members::find(db, team, viewer).await?.is_none() {
		return Err(ServerError::NotFound);
	}
	super::get_team(db, team)
		.await?
		.ok_or(ServerError::NotFound)
}

/// A person's picture for `viewer`: their own, or one of somebody they share a team with.
pub async fn user_image(db: &PgPool, viewer: UserId, user: UserId) -> AppResult<Stored> {
	if !super::may_see_user(db, viewer, user).await? {
		return Err(ServerError::NotFound);
	}
	super::get_user(db, user)
		.await?
		.ok_or(ServerError::NotFound)
}
