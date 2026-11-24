//! SQL the git provider page tests need and the UI may not write itself.

use super::PgPool;

/// The id of the provider called `name`.
pub async fn provider_id(db: &PgPool, name: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM git_providers WHERE name = $1")
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}

/// `(provider, repository, clone URL)` of the application with `slug`.
pub async fn app_provider(
	db: &PgPool,
	slug: &str,
) -> (Option<i64>, Option<String>, Option<String>) {
	sqlx::query_as("SELECT git_provider_id, git_repo, repo_url FROM applications WHERE slug = $1")
		.bind(slug)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Removes every application, so a provider they used can be deleted.
pub async fn delete_applications(db: &PgPool) {
	sqlx::query("DELETE FROM applications")
		.execute(db)
		.await
		.unwrap();
}
