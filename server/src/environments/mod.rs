//! Environments: the level between a project and its resources. Every
//! application, database and model lives in exactly one environment of its
//! project (`production`, `staging`, ...).
//!
//! A project's `production` environment is made by a database trigger on
//! `projects`, so it exists from the same statement that creates the project.
//! The composite foreign key `(environment_id, project_id)` on each resource
//! table keeps a resource out of another project's environment.

mod routes;
pub mod service;

pub use routes::router;

#[cfg(test)]
mod tests;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::{EnvironmentId, ProjectId};

#[derive(Debug, Clone)]
pub struct Environment {
	pub id: EnvironmentId,
	pub project_id: ProjectId,
	pub name: String,
	/// URL-safe identifier, unique within the project.
	pub slug: String,
	/// Display order within the project; the lowest is the default.
	pub position: i32,
	pub created_at: OffsetDateTime,
}

pub async fn list_by_project(
	db: impl PgExecutor<'_>,
	project: ProjectId,
) -> Result<Vec<Environment>, sqlx::Error> {
	sqlx::query_as!(
		Environment,
		r#"
			SELECT id AS "id: EnvironmentId", project_id AS "project_id: ProjectId",
				name, slug, position, created_at
			FROM environments
			WHERE project_id = $1
			ORDER BY position, id
		"#,
		project as ProjectId
	)
	.fetch_all(db)
	.await
}

/// The project's default environment: the first by position.
pub async fn first(
	db: impl PgExecutor<'_>,
	project: ProjectId,
) -> Result<Option<EnvironmentId>, sqlx::Error> {
	sqlx::query_scalar!(
		r#"
			SELECT id AS "id: EnvironmentId" FROM environments
			WHERE project_id = $1
			ORDER BY position, id
			LIMIT 1
		"#,
		project as ProjectId
	)
	.fetch_optional(db)
	.await
}

pub async fn insert(
	db: impl PgExecutor<'_>,
	project: ProjectId,
	name: &str,
	slug: &str,
) -> Result<Environment, sqlx::Error> {
	sqlx::query_as!(
		Environment,
		r#"
			INSERT INTO environments (id, project_id, name, slug, position)
			VALUES ($1, $2, $3, $4,
				(SELECT COALESCE(MAX(position) + 1, 0) FROM environments WHERE project_id = $2))
			RETURNING id AS "id: EnvironmentId", project_id AS "project_id: ProjectId",
				name, slug, position, created_at
		"#,
		EnvironmentId::generate() as EnvironmentId,
		project as ProjectId,
		name,
		slug
	)
	.fetch_one(db)
	.await
}

pub async fn update(
	db: impl PgExecutor<'_>,
	id: EnvironmentId,
	name: &str,
	slug: &str,
) -> Result<Environment, sqlx::Error> {
	sqlx::query_as!(
		Environment,
		r#"
			UPDATE environments SET name = $2, slug = $3
			WHERE id = $1
			RETURNING id AS "id: EnvironmentId", project_id AS "project_id: ProjectId",
				name, slug, position, created_at
		"#,
		id as EnvironmentId,
		name,
		slug
	)
	.fetch_one(db)
	.await
}

pub async fn set_position(
	db: impl PgExecutor<'_>,
	id: EnvironmentId,
	position: i32,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE environments SET position = $2 WHERE id = $1",
		id as EnvironmentId,
		position
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, id: EnvironmentId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM environments WHERE id = $1",
		id as EnvironmentId
	)
	.execute(db)
	.await?;
	Ok(())
}

/// What lives in an environment, as `(kind, name)` pairs: applications, then
/// databases, then models.
pub async fn resources(
	db: impl PgExecutor<'_>,
	id: EnvironmentId,
) -> Result<Vec<(String, String)>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT kind AS "kind!", name AS "name!" FROM (
				SELECT 'application' AS kind, name, 0 AS ord, id FROM applications
					WHERE environment_id = $1
				UNION ALL
				SELECT 'database', name, 1, id FROM databases WHERE environment_id = $1
				UNION ALL
				SELECT 'model', name, 2, id FROM models WHERE environment_id = $1
			) r
			ORDER BY ord, id
		"#,
		id as EnvironmentId
	)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(|r| (r.kind, r.name)).collect())
}

/// The optional `?environment_id=` of a resource list endpoint.
#[derive(Debug, Default, serde::Deserialize)]
pub struct EnvironmentFilter {
	#[serde(default)]
	pub environment_id: Option<EnvironmentId>,
}
