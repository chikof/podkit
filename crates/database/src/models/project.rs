use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct ProjectModel {
	pub id: i64,
	pub team_id: i64,
	pub name: String,
	pub slug: String,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewProject {
	pub id: i64,
	pub team_id: i64,
	pub name: String,
	pub slug: String,
}

impl ProjectModel {
	pub async fn create<'e>(
		exec: impl DbExecutor<'e>,
		new: NewProject,
	) -> Result<Self, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			r#"
				INSERT INTO projects (id, team_id, name, slug)
				VALUES ($1, $2, $3, $4)
				RETURNING *
			"#,
			new.id,
			new.team_id,
			new.name,
			new.slug
		)
		.fetch_one(exec)
		.await?)
	}

	pub async fn find_by_id<'e>(
		exec: impl DbExecutor<'e>,
		id: i64,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM projects WHERE id = $1", id)
				.fetch_optional(exec)
				.await?,
		)
	}

	pub async fn list_by_team<'e>(
		exec: impl DbExecutor<'e>,
		team_id: i64,
	) -> Result<Vec<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM projects WHERE team_id = $1", team_id)
				.fetch_all(exec)
				.await?,
		)
	}

	pub async fn delete<'e>(exec: impl DbExecutor<'e>, id: i64) -> Result<(), DatabaseError> {
		sqlx::query!("DELETE FROM projects WHERE id = $1", id)
			.execute(exec)
			.await?;
		Ok(())
	}
}
