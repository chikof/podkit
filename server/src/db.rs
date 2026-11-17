//! Postgres pool construction, migrations, and error helpers.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
	PgPoolOptions::new().max_connections(10).connect(url).await
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
