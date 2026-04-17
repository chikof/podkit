use sqlx::prelude::FromRow;
use time::OffsetDateTime;

#[derive(Debug, Clone, FromRow)]
pub struct UserModel {
	id: String,
	name: String,
	email: String,
	password_hash: String,
	created_at: OffsetDateTime,
	updated_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct NewUser {
	pub email: String,
}
