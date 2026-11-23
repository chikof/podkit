//! Raw-SQL helpers for the uptime page tests. They live here because the UI never touches the
//! database, tests included.

use sqlx::PgPool;

/// Puts the check called `name` into `state` (`up`, `down`, `unknown`), as the runner would.
pub async fn set_state(db: &PgPool, name: &str, state: &str) {
	sqlx::query("UPDATE uptime_checks SET state = $2, state_since = NOW() WHERE name = $1")
		.bind(name)
		.bind(state)
		.execute(db)
		.await
		.unwrap();
}

/// Gives the check called `name` `ok` good and `total - ok` bad results `days_ago` days back,
/// at noon UTC, as the runner would have stored them.
pub async fn add_results(db: &PgPool, name: &str, days_ago: i32, total: i32, ok: i32) {
	for n in 0..total {
		sqlx::query(
			"INSERT INTO uptime_results (check_id, at, ok, latency_ms)
			SELECT id, date_trunc('day', NOW()) - make_interval(days => $2) + make_interval(mins => $3),
				$4, CASE WHEN $4 THEN 80 END
			FROM uptime_checks WHERE name = $1",
		)
		.bind(name)
		.bind(days_ago)
		.bind(n)
		.bind(n < ok)
		.execute(db)
		.await
		.unwrap();
	}
}

/// The check called `name`'s state.
pub async fn state_of(db: &PgPool, name: &str) -> String {
	sqlx::query_scalar("SELECT state FROM uptime_checks WHERE name = $1")
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}

/// The id of the check called `name`.
pub async fn check_id(db: &PgPool, name: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM uptime_checks WHERE name = $1")
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}
