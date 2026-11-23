//! Team use cases shared by the JSON API and the web UI.

use sqlx::PgPool;

use super::Team;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};
use crate::validate;

/// Constraint violated when a slug is taken.
const SLUG_UNIQUE: &str = "teams_slug_key";

/// Creates a team and makes `owner` its Owner. Any signed-in user may do this.
pub async fn create(
	db: &PgPool,
	owner: UserId,
	name: &str,
	slug: &str,
	logo: &str,
) -> AppResult<Team> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > 60 {
		return Err(ServerError::Validation(
			"Name must be 1 to 60 characters.".to_owned(),
		));
	}
	if !validate::slug(slug) {
		return Err(ServerError::Validation(
			"Slug must be 1 to 60 lowercase letters, digits or hyphens, and not start or end with a hyphen."
				.to_owned(),
		));
	}

	let team = super::create_with_owner(db, name, slug, logo, owner)
		.await
		.map_err(|e| {
			if is_unique_violation(&e, SLUG_UNIQUE) {
				ServerError::Conflict("A team with that slug already exists.".to_owned())
			} else {
				e.into()
			}
		})?;
	audit::log(
		db,
		owner,
		team.id,
		"team.create",
		Target::new("team", team.id).named(&team.slug),
		Diff::new().set("name", team.name.clone()),
	)
	.await;
	Ok(team)
}

/// Renames a team; the slug stays. Needs `update team`.
///
/// # Errors
/// `Forbidden` without the permission, `Validation` for a name that is blank or longer
/// than 60 characters.
pub async fn rename(db: &PgPool, user: UserId, team: TeamId, name: &str) -> AppResult<Team> {
	authorize(db, user, team, Action::Update, Resource::Team).await?;
	let name = name.trim();
	if name.is_empty() || name.chars().count() > 60 {
		return Err(ServerError::Validation(
			"Name must be 1 to 60 characters.".to_owned(),
		));
	}
	let old = sqlx::query_scalar!("SELECT name FROM teams WHERE id = $1", team as TeamId)
		.fetch_one(db)
		.await?;
	let renamed = sqlx::query_as!(
		Team,
		r#"
			UPDATE teams SET name = $2 WHERE id = $1
			RETURNING id AS "id: TeamId", name, slug, logo,
				(SELECT hash FROM team_avatars WHERE team_id = teams.id) AS avatar
		"#,
		team as TeamId,
		name
	)
	.fetch_one(db)
	.await?;
	audit::log(
		db,
		user,
		team,
		"team.rename",
		Target::new("team", team).named(&renamed.slug),
		Diff::new().change("name", old, renamed.name.clone()),
	)
	.await;
	Ok(renamed)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ids::UserId;
	use crate::tests::{PgPool, app, db_test, user_id, user_with_team};

	#[db_test]
	async fn an_owner_renames_and_the_slug_stays(db: PgPool) {
		let app = app(db.clone()).await;
		let (_, team) = user_with_team(&app, "alice").await;
		let alice = UserId(user_id(&db, "alice").await);

		let renamed = rename(&db, alice, TeamId(team), "  Alice & Co  ")
			.await
			.unwrap();
		assert_eq!(renamed.name, "Alice & Co");
		assert_eq!(renamed.slug, "alice");

		for bad in ["", "   ", &"x".repeat(61)] {
			let err = rename(&db, alice, TeamId(team), bad).await.unwrap_err();
			assert!(
				matches!(err, ServerError::Validation(_)),
				"{bad:?}: {err:?}"
			);
		}
	}

	#[db_test]
	async fn a_stranger_cannot_rename(db: PgPool) {
		let app = app(db.clone()).await;
		let (_, team) = user_with_team(&app, "alice").await;
		user_with_team(&app, "bob").await;
		let bob = UserId(user_id(&db, "bob").await);

		let err = rename(&db, bob, TeamId(team), "Mine now")
			.await
			.unwrap_err();
		assert!(matches!(err, ServerError::Forbidden), "{err:?}");
		let name: String = sqlx::query_scalar("SELECT name FROM teams WHERE id = $1")
			.bind(team)
			.fetch_one(&db)
			.await
			.unwrap();
		assert_eq!(name, "alice");
	}
}
