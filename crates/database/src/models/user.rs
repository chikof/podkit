use sqlx::{prelude::FromRow, PgPool};
use time::OffsetDateTime;

use crypto::{ids::generate_id, passwords};
use zeroize::Zeroizing;

use crate::{DatabaseError, DbExecutor};

#[derive(Debug, Clone, FromRow)]
pub struct UserModel {
	pub id: String,
	pub name: String,
	pub email: String,
	pub password_hash: String,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewUser {
	pub email: String,
	pub name: String,
	pub pasword: String,
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
			generate_id(None),
			new.name,
			new.email,
			new.pasword
		)
		.fetch_one(exec)
		.await?)
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

	pub async fn authenticate(
		pool: &PgPool,
		email: &str,
		password: Zeroizing<String>,
	) -> Result<Option<Self>, DatabaseError> {
		let Some(user) = Self::find_by_email(pool, email).await? else {
			// Run verify anyway to prevent timing-based user enumeration
			passwords::verify(password, "$argon2id$dummy".into())
				.await
				.ok();
			return Ok(None);
		};

		let valid = passwords::verify(password, user.password_hash.clone()).await?;

		Ok(valid.then_some(user))
	}
}
