use std::sync::OnceLock;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres};

use crate::DatabaseError;

static CONNECTION: OnceLock<Pool<Postgres>> = OnceLock::new();

/// This obtains a database connection from the `CONNECTION` oncelock
/// or creates a new connection and stores it there.
///
/// This is not to be used directly, prefer `db!()` instead.
pub async fn get_db_connection<'r>(url: Option<&str>) -> Result<&'r Pool<Postgres>, DatabaseError> {
	if let Some(connection) = CONNECTION.get() {
		return Ok(connection);
	}

	let pool = PgPoolOptions::new()
    .max_connections(5)
    .connect(
        url.unwrap_or(&std::env::var("DATABASE_URL")?) // omg I hated env! stopping my compilation
    )
    .await?;

	Ok(CONNECTION.get_or_init(|| pool))
}

pub async fn migrate() -> Result<(), DatabaseError> {
	if let Some(pool) = CONNECTION.get() {
		sqlx::migrate!().run(pool).await?;
		return Ok(());
	}

	Err(DatabaseError::MigrationError)
}
