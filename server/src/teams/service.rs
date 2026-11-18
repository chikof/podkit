//! Team use cases shared by the JSON API and the web UI.

use sqlx::PgPool;

use super::Team;
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::UserId;
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

	super::create_with_owner(db, name, slug, logo, owner)
		.await
		.map_err(|e| {
			if is_unique_violation(&e, SLUG_UNIQUE) {
				ServerError::Conflict("A team with that slug already exists.".to_owned())
			} else {
				e.into()
			}
		})
}
