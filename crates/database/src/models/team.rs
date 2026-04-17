use nanoid::nanoid;
use sqlx::error::DatabaseError;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crypto::ids::generate_id;

use crate::DbExecutor;

#[derive(Debug, Clone, FromRow)]
pub struct TeamModel {
	id: String,
	name: String,
	slug: String,
	logo: String,
	created_at: OffsetDateTime,
	owner_id: String,
}

#[derive(Debug)]
pub struct NewTeam {
	pub name: String,
	pub logo: String,
	pub owner_id: String,
}

impl TeamModel {
	pub async fn create<'e>(exec: impl DbExecutor<'e>, new: NewTeam) -> DatabaseError {
		sqlx::query_as!(
			Self,
			r#"
				INSERT INTO teams (id, name, logo, owner_id)
				VALUES (generate_id(None), $1, $2, $3)
			"#,
			new.name,
			new.logo,
			new.owner_id
		)
		.fetch_one(exec)
		.await
	}
}
