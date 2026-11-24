//! SQL the health tests of the pages need and the UI may not write itself.

use super::PgPool;

/// `(kind, path, expected status, interval, rollback window)` of the application's health
/// settings, `None` when it has none.
pub async fn settings(db: &PgPool, application: i64) -> Option<(String, String, i32, i32, i32)> {
	sqlx::query_as(
		"SELECT kind, path, expect_status, interval_s, rollback_window_min
		 FROM application_health WHERE application_id = $1",
	)
	.bind(application)
	.fetch_optional(db)
	.await
	.unwrap()
}

/// Records what the health gate says about deployment `id`.
pub async fn seed_gate(
	db: &PgPool,
	id: i64,
	state: &str,
	detail: Option<&str>,
	replaces: Option<i64>,
) {
	sqlx::query(
		"INSERT INTO deployment_health (deployment_id, state, detail, rollback_of)
		 VALUES ($1, $2, $3, $4)",
	)
	.bind(id)
	.bind(state)
	.bind(detail)
	.bind(replaces)
	.execute(db)
	.await
	.unwrap();
}
