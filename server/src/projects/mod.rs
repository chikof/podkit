//! Projects: group a team's applications.

pub mod overview;
mod routes;
pub mod service;

pub use routes::router;

use crate::ids::{ProjectId, TeamId};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Project {
	pub id: ProjectId,
	pub team_id: TeamId,
	pub name: String,
	/// URL-safe unique identifier, scoped to the team.
	pub slug: String,
}

pub async fn list_by_team(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<service::ProjectSummary>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT p.id AS "id: ProjectId", p.team_id AS "team_id: TeamId", p.name, p.slug
			FROM projects p
			WHERE p.team_id = $1
			ORDER BY p.name, p.id
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| service::ProjectSummary {
			project: Project {
				id: r.id,
				team_id: r.team_id,
				name: r.name,
				slug: r.slug,
			},
		})
		.collect())
}

pub async fn insert(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	name: &str,
	slug: &str,
) -> Result<Project, sqlx::Error> {
	sqlx::query_as!(
		Project,
		r#"
			INSERT INTO projects (id, team_id, name, slug)
			VALUES ($1, $2, $3, $4)
			RETURNING id AS "id: ProjectId", team_id AS "team_id: TeamId", name, slug
		"#,
		ProjectId::generate() as ProjectId,
		team as TeamId,
		name,
		slug
	)
	.fetch_one(db)
	.await
}

pub async fn delete(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	id: ProjectId,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM projects WHERE id = $1 AND team_id = $2",
		id as ProjectId,
		team as TeamId
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn update_name(
	db: impl sqlx::PgExecutor<'_>,
	id: ProjectId,
	name: &str,
) -> Result<Project, sqlx::Error> {
	sqlx::query_as!(
		Project,
		r#"
			UPDATE projects SET name = $2 WHERE id = $1
			RETURNING id AS "id: ProjectId", team_id AS "team_id: TeamId", name, slug
		"#,
		id as ProjectId,
		name
	)
	.fetch_one(db)
	.await
}
