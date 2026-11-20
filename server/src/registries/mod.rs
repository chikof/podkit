//! Saved registries: a team's container-registry logins. Image apps and stacks pick one
//! instead of carrying their own credentials. Routes live under
//! `/api/v1/teams/{team}/registries`; the use cases are in [`service`].
//!
//! The password is write-only: it is encrypted with the server's secret box on the way in
//! and decrypted only to build a deploy spec or to try a login.

mod routes;
pub mod service;
#[cfg(test)]
mod tests;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::{RegistryId, TeamId};

pub use routes::router;

/// Row from the `registries` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Registry {
	pub id: RegistryId,
	pub name: String,
	/// Host and optional port, as a pull names it: `ghcr.io`, `127.0.0.1:5000`.
	pub url: String,
	pub username: String,
	/// age-encrypted password.
	pub password: Vec<u8>,
	pub created_at: OffsetDateTime,
}

/// Unique constraint on `(team_id, name)`.
pub const NAME_CONSTRAINT: &str = "registries_team_id_name_key";
/// Unique constraint on `(team_id, url, username)`.
pub const LOGIN_CONSTRAINT: &str = "registries_team_id_url_username_key";

pub async fn list_by_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<Registry>, sqlx::Error> {
	sqlx::query_as!(
		Registry,
		r#"
			SELECT id AS "id: RegistryId", name, url, username,
				password, created_at
			FROM registries WHERE team_id = $1 ORDER BY name
		"#,
		team as TeamId,
	)
	.fetch_all(db)
	.await
}

/// By id alone, no team check: for deploy jobs, which act for a resource already
/// resolved inside its team. Handlers use [`crate::scope::registry`].
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: RegistryId,
) -> Result<Option<Registry>, sqlx::Error> {
	sqlx::query_as!(
		Registry,
		r#"
			SELECT id AS "id: RegistryId", name, url, username,
				password, created_at
			FROM registries WHERE id = $1
		"#,
		id as RegistryId,
	)
	.fetch_optional(db)
	.await
}

/// The team's registries with how many applications and stacks pull with each.
pub async fn list_with_usage(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<(Registry, i64)>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT r.id AS "id: RegistryId", r.name, r.url, r.username, r.password, r.created_at,
				(SELECT COUNT(*) FROM applications a WHERE a.registry_id = r.id)
				+ (SELECT COUNT(*) FROM stacks s WHERE s.registry_id = r.id) AS "used_by!"
			FROM registries r WHERE r.team_id = $1 ORDER BY r.name
		"#,
		team as TeamId,
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| {
			let registry = Registry {
				id: r.id,
				name: r.name,
				url: r.url,
				username: r.username,
				password: r.password,
				created_at: r.created_at,
			};
			(registry, r.used_by)
		})
		.collect())
}
