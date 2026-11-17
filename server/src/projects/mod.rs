//! Projects: group a team's applications.

mod routes;

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

pub async fn list_by_team(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<Project>, sqlx::Error> {
	sqlx::query_as!(
		Project,
		r#"
			SELECT id AS "id: ProjectId", team_id AS "team_id: TeamId", name, slug
			FROM projects
			WHERE team_id = $1
		"#,
		team as TeamId
	)
	.fetch_all(db)
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
