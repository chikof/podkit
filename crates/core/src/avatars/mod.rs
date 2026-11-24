//! Profile pictures for teams and people. Uploads are decoded, cropped square, resized and
//! re-encoded by the server (see [`process`]), then kept in Postgres so every control-plane
//! server serves the same picture. Nothing the browser sent is ever stored or served as is.

mod process;
mod routes;
pub mod service;
#[cfg(test)]
mod tests;

pub use process::{MAX_UPLOAD, Processed};
pub use routes::{api_router, public_router, read_upload};

use sqlx::PgExecutor;

use crate::ids::{TeamId, UserId};

/// A stored picture, as served.
#[derive(Debug, Clone)]
pub struct Stored {
	pub mime: String,
	pub bytes: Vec<u8>,
	pub hash: String,
}

/// Where a team's picture is served; `hash` busts the browser's cache when it changes.
pub fn team_url(team: TeamId, hash: &str) -> String {
	format!("/avatars/team/{team}?v={hash}")
}

/// Where a person's picture is served.
pub fn user_url(user: UserId, hash: &str) -> String {
	format!("/avatars/user/{user}?v={hash}")
}

async fn put_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
	image: &Processed,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO team_avatars (team_id, mime, bytes, hash)
			VALUES ($1, $2, $3, $4)
			ON CONFLICT (team_id) DO UPDATE
			SET mime = $2, bytes = $3, hash = $4, updated_at = NOW()
		"#,
		team as TeamId,
		image.mime,
		image.bytes,
		image.hash
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn delete_team(db: impl PgExecutor<'_>, team: TeamId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM team_avatars WHERE team_id = $1",
		team as TeamId
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn get_team(db: impl PgExecutor<'_>, team: TeamId) -> Result<Option<Stored>, sqlx::Error> {
	sqlx::query_as!(
		Stored,
		"SELECT mime, bytes, hash FROM team_avatars WHERE team_id = $1",
		team as TeamId
	)
	.fetch_optional(db)
	.await
}

async fn put_user(
	db: impl PgExecutor<'_>,
	user: UserId,
	image: &Processed,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO user_avatars (user_id, mime, bytes, hash)
			VALUES ($1, $2, $3, $4)
			ON CONFLICT (user_id) DO UPDATE
			SET mime = $2, bytes = $3, hash = $4, updated_at = NOW()
		"#,
		user as UserId,
		image.mime,
		image.bytes,
		image.hash
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn delete_user(db: impl PgExecutor<'_>, user: UserId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM user_avatars WHERE user_id = $1",
		user as UserId
	)
	.execute(db)
	.await?;
	Ok(())
}

async fn get_user(db: impl PgExecutor<'_>, user: UserId) -> Result<Option<Stored>, sqlx::Error> {
	sqlx::query_as!(
		Stored,
		"SELECT mime, bytes, hash FROM user_avatars WHERE user_id = $1",
		user as UserId
	)
	.fetch_optional(db)
	.await
}

/// True when `viewer` is `target` or belongs to a team `target` belongs to.
async fn may_see_user(
	db: impl PgExecutor<'_>,
	viewer: UserId,
	target: UserId,
) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar!(
		r#"
			SELECT ($1::BIGINT = $2::BIGINT) OR EXISTS (
				SELECT 1 FROM team_members a
				JOIN team_members b ON b.team_id = a.team_id
				WHERE a.user_id = $1 AND b.user_id = $2
			) AS "visible!"
		"#,
		viewer as UserId,
		target as UserId
	)
	.fetch_one(db)
	.await
}
