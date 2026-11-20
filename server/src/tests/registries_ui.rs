//! Raw-SQL helpers for the registry, API token and registry-picker page tests. They live
//! here because the UI never touches the database, tests included.

use sqlx::PgPool;

/// The only saved registry's id.
pub async fn registry_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM registries")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The only saved registry's stored (encrypted) password.
pub async fn registry_password(db: &PgPool) -> Vec<u8> {
	sqlx::query_scalar("SELECT password FROM registries")
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn registry_name(db: &PgPool) -> String {
	sqlx::query_scalar("SELECT name FROM registries")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The registry the only stack pulls with.
pub async fn stack_registry(db: &PgPool) -> Option<i64> {
	sqlx::query_scalar("SELECT registry_id FROM stacks")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The only API token's id.
pub async fn token_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM api_tokens")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The only API token's stored hash.
pub async fn token_hash(db: &PgPool) -> Vec<u8> {
	sqlx::query_scalar("SELECT hash FROM api_tokens")
		.fetch_one(db)
		.await
		.unwrap()
}
