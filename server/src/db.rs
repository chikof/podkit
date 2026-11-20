//! Postgres pool construction, migrations, and error helpers.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Connections the pool may hold at most.
const MAX_CONNECTIONS: u32 = 5;

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
	// Every Postgres backend costs the database server a few MB, so the pool is
	// small and gives idle connections back after half a minute.
	PgPoolOptions::new()
		.max_connections(MAX_CONNECTIONS)
		.min_connections(0)
		.idle_timeout(std::time::Duration::from_secs(30))
		.connect(url)
		.await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
	sqlx::migrate!().run(pool).await
}

/// True when `e` is a unique violation (`23505`) on `constraint`.
pub fn is_unique_violation(e: &sqlx::Error, constraint: &str) -> bool {
	e.as_database_error()
		.is_some_and(|d| d.code().as_deref() == Some("23505") && d.constraint() == Some(constraint))
}

/// True when `e` says a row is still referenced: a foreign key violation
/// (`23503`) or a `RESTRICT` one (`23001`).
pub fn is_reference_violation(e: &sqlx::Error) -> bool {
	e.as_database_error()
		.is_some_and(|d| matches!(d.code().as_deref(), Some("23503" | "23001")))
}
