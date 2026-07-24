use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct RoleModel {
	pub id: i64,
	pub team_id: Option<i64>,
	pub name: String,
	pub is_default: bool,
	pub created_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewRole {
	pub id: i64,
	pub team_id: Option<i64>,
	pub name: String,
	pub is_default: bool,
}

impl RoleModel {
	pub async fn find_by_id<'e>(
		exec: impl DbExecutor<'e>,
		id: i64,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM roles WHERE id = $1", id)
				.fetch_optional(exec)
				.await?,
		)
	}

	pub async fn find_builtin_by_name<'e>(
		exec: impl DbExecutor<'e>,
		name: &str,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			"SELECT * FROM roles WHERE team_id IS NULL AND name = $1",
			name
		)
		.fetch_optional(exec)
		.await?)
	}

	pub async fn list_for_team<'e>(
		exec: impl DbExecutor<'e>,
		team_id: i64,
	) -> Result<Vec<Self>, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			"SELECT * FROM roles WHERE team_id IS NULL OR team_id = $1",
			team_id
		)
		.fetch_all(exec)
		.await?)
	}

	pub async fn create<'e>(
		exec: impl DbExecutor<'e>,
		new: NewRole,
	) -> Result<Self, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			r#"
				INSERT INTO roles (id, team_id, name, is_default)
				VALUES ($1, $2, $3, $4)
				RETURNING *
			"#,
			new.id,
			new.team_id,
			new.name,
			new.is_default
		)
		.fetch_one(exec)
		.await?)
	}

	pub async fn delete<'e>(exec: impl DbExecutor<'e>, id: i64) -> Result<(), DatabaseError> {
		sqlx::query!("DELETE FROM roles WHERE id = $1", id)
			.execute(exec)
			.await?;
		Ok(())
	}
}
