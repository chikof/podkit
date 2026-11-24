//! SQL the application page tests need and the UI may not write itself.

use super::PgPool;

/// `(source kind, image, whether registry credentials are stored)` of the application.
pub async fn app_source(db: &PgPool, slug: &str) -> (String, Option<String>, bool) {
	sqlx::query_as(
		"SELECT source_kind::text, image, registry_auth IS NOT NULL FROM applications WHERE slug = $1",
	)
	.bind(slug)
	.fetch_one(db)
	.await
	.unwrap()
}

/// The environment the application lives in.
pub async fn app_environment(db: &PgPool, slug: &str) -> i64 {
	sqlx::query_scalar("SELECT environment_id FROM applications WHERE slug = $1")
		.bind(slug)
		.fetch_one(db)
		.await
		.unwrap()
}

/// The stored (encrypted) webhook secret of the application.
pub async fn webhook_secret_blob(db: &PgPool, slug: &str) -> Vec<u8> {
	sqlx::query_scalar("SELECT webhook_secret FROM applications WHERE slug = $1")
		.bind(slug)
		.fetch_one(db)
		.await
		.unwrap()
}
