use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct TeamMemberModel {
	pub id: i64,
	pub team_id: i64,
	pub user_id: i64,
	pub role_id: i64,
	pub joined_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewTeamMember {
	pub id: i64,
	pub team_id: i64,
	pub user_id: i64,
	pub role_id: i64,
}

impl TeamMemberModel {
	/// Inserts a membership, or updates its `role_id` if a row with this `id` already
	/// exists (this is how role changes are persisted: fetch, mutate then save).
	pub async fn create<'e>(
		exec: impl DbExecutor<'e>,
		new: NewTeamMember,
	) -> Result<Self, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			r#"
				INSERT INTO team_members (id, team_id, user_id, role_id)
				VALUES ($1, $2, $3, $4)
				ON CONFLICT (id) DO UPDATE SET role_id = EXCLUDED.role_id
				RETURNING *
			"#,
			new.id,
			new.team_id,
			new.user_id,
			new.role_id
		)
		.fetch_one(exec)
		.await?)
	}

	pub async fn find_by_team_and_user<'e>(
		exec: impl DbExecutor<'e>,
		team_id: i64,
		user_id: i64,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			"SELECT * FROM team_members WHERE team_id = $1 AND user_id = $2",
			team_id,
			user_id
		)
		.fetch_optional(exec)
		.await?)
	}

	pub async fn list_by_team<'e>(
		exec: impl DbExecutor<'e>,
		team_id: i64,
	) -> Result<Vec<Self>, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			"SELECT * FROM team_members WHERE team_id = $1",
			team_id
		)
		.fetch_all(exec)
		.await?)
	}

	pub async fn update_role<'e>(
		exec: impl DbExecutor<'e>,
		team_id: i64,
		user_id: i64,
		role_id: i64,
	) -> Result<(), DatabaseError> {
		sqlx::query!(
			"UPDATE team_members SET role_id = $1 WHERE team_id = $2 AND user_id = $3",
			role_id,
			team_id,
			user_id
		)
		.execute(exec)
		.await?;
		Ok(())
	}

	pub async fn delete<'e>(
		exec: impl DbExecutor<'e>,
		team_id: i64,
		user_id: i64,
	) -> Result<(), DatabaseError> {
		sqlx::query!(
			"DELETE FROM team_members WHERE team_id = $1 AND user_id = $2",
			team_id,
			user_id
		)
		.execute(exec)
		.await?;
		Ok(())
	}
}
