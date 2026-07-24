use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct UserModel {
	pub id: i64,
	pub name: String,
	pub email: String,
	pub password_hash: String,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewUser {
	pub id: i64,
	pub email: String,
	pub name: String,
	pub password_hash: String,
}

impl UserModel {
	/// Creates a new user in the database
	///
	/// # Errors
	/// - Fails when theres an existing using with the same email.
	pub async fn create<'e>(
		exec: impl DbExecutor<'e>,
		new: NewUser,
	) -> Result<Self, DatabaseError> {
		Ok(sqlx::query_as!(
			Self,
			r#"
				INSERT INTO users (id, name, email, password_hash)
				VALUES ($1, $2, $3, $4)
				RETURNING *
			"#,
			new.id,
			new.name,
			new.email,
			new.password_hash
		)
		.fetch_one(exec)
		.await?)
	}

	pub async fn find_by_id<'e>(
		exec: impl DbExecutor<'e>,
		id: i64,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM users WHERE id = $1", id)
				.fetch_optional(exec)
				.await?,
		)
	}

	pub async fn find_by_email<'e>(
		exec: impl DbExecutor<'e>,
		email: &str,
	) -> Result<Option<Self>, DatabaseError> {
		Ok(
			sqlx::query_as!(Self, "SELECT * FROM users WHERE email = $1", email)
				.fetch_optional(exec)
				.await?,
		)
	}

	pub async fn exists_by_email<'e>(
		exec: impl DbExecutor<'e>,
		email: &str,
	) -> Result<bool, DatabaseError> {
		let exists = sqlx::query_scalar!(
			r#"SELECT EXISTS(SELECT 1 FROM users WHERE email = $1) AS "exists!""#,
			email
		)
		.fetch_one(exec)
		.await?;
		Ok(exists)
	}

	pub async fn delete<'e>(exec: impl DbExecutor<'e>, id: i64) -> Result<(), DatabaseError> {
		sqlx::query!("DELETE FROM users WHERE id = $1", id)
			.execute(exec)
			.await?;
		Ok(())
	}
}
