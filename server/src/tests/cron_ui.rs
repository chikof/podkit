//! Raw-SQL helpers for the scheduled-task page tests. They live here because the UI never
//! touches the database, tests included.

use sqlx::PgPool;

/// The slug of an environment.
pub async fn env_slug(db: &PgPool, env: i64) -> String {
	sqlx::query_scalar("SELECT slug FROM environments WHERE id = $1")
		.bind(env)
		.fetch_one(db)
		.await
		.unwrap()
}

/// A task's name, schedule, overlap policy and retries.
pub async fn settings(db: &PgPool, task: i64) -> (String, String, String, i32) {
	sqlx::query_as("SELECT name, schedule, concurrency, max_retries FROM cron_tasks WHERE id = $1")
		.bind(task)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Whether a task is enabled.
pub async fn enabled(db: &PgPool, task: i64) -> bool {
	sqlx::query_scalar("SELECT enabled FROM cron_tasks WHERE id = $1")
		.bind(task)
		.fetch_one(db)
		.await
		.unwrap()
}
