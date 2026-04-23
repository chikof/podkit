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
	pub owner_id: i64,
}

#[derive(Debug)]
pub struct NewTeam {
	pub name: String,
	pub logo: String,
	pub owner_id: i64,
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
				INSERT INTO teams (id, name, logo, owner_id)
				VALUES ($1, $2, $3, $4)
				RETURNING *
			"#,
			generate_id(),
			new.name,
			new.logo,
			new.owner_id
		)
		.fetch_one(exec)
		.await?)
	}
}
