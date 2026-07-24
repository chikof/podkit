use sqlx::PgPool;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crypto::generate_id;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct TeamModel {
	pub id: i64,
	pub name: String,
	pub slug: String,
	pub logo: String,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewTeam {
	pub id: i64,
	pub name: String,
	pub slug: String,
	pub logo: String,
}

impl TeamModel {
	/// Creates a team in the database
	///
	/// # Errors
	/// - It shouldn't fail unless sqlx decices theres an error
	pub async fn create<'e>(
		exec: impl DbExecutor<'e>,
		new: NewTeam,
	) -> Result<Self, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			r#"
				INSERT INTO teams (id, name, slug, logo)
				VALUES ($1, $2, $3, $4)
				RETURNING *
			"#,
			new.id,
			new.name,
			new.slug,
			new.logo
		)
		.fetch_one(exec)
		.await?)
	}

	pub async fn find_by_id<'e>(
		exec: impl DbExecutor<'e>,
		id: i64,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM teams WHERE id = $1", id)
				.fetch_optional(exec)
				.await?,
		)
	}

	pub async fn find_by_slug<'e>(
		exec: impl DbExecutor<'e>,
		slug: &str,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM teams WHERE slug = $1", slug)
				.fetch_optional(exec)
				.await?,
		)
	}

	pub async fn delete<'e>(exec: impl DbExecutor<'e>, id: i64) -> Result<(), DatabaseError> {
		sqlx::query!("DELETE FROM teams WHERE id = $1", id)
			.execute(exec)
			.await?;
		Ok(())
	}

	pub async fn list<'e>(
		exec: impl DbExecutor<'e>,
		issuer_id: i64,
	) -> Result<Vec<Self>, DatabaseError> {
		Ok(sqlx::query_as!(Self, "SELECT teams.id, teams.name, teams.slug, teams.logo, teams.created_at, teams.updated_at FROM teams JOIN team_members ON (teams.id = team_members.team_id) WHERE team_members.user_id = $1", issuer_id)
			.fetch_all(exec)
			.await?)
	}

	/// Creates a team and assigns `owner_user_id` the global `Owner` role, in one transaction.
	///
	/// # Errors
	/// Fails if the transaction can't be committed, or if the `Owner` role is missing
	/// (it's seeded by migration `0009_seed_rbac_defaults` and should always exist).
	pub async fn create_with_owner(
		pool: &PgPool,
		name: String,
		slug: String,
		logo: String,
		owner_user_id: i64,
	) -> Result<Self, DatabaseError> {
		let mut tx = pool.begin().await?;

		let team = sqlx::query_as!(
			Self,
			r#"
				INSERT INTO teams (id, name, slug, logo)
				VALUES ($1, $2, $3, $4)
				RETURNING *
			"#,
			generate_id(),
			name,
			slug,
			logo
		)
		.fetch_one(&mut *tx)
		.await?;

		let owner_role_id: i64 =
			sqlx::query_scalar!("SELECT id FROM roles WHERE team_id IS NULL AND name = 'Owner'")
				.fetch_one(&mut *tx)
				.await?;

		sqlx::query!(
			"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, $4)",
			generate_id(),
			team.id,
			owner_user_id,
			owner_role_id
		)
		.execute(&mut *tx)
		.await?;

		tx.commit().await?;

		Ok(team)
	}
}
