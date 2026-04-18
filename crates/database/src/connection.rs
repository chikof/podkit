use std::sync::OnceLock;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres};

use crate::DatabaseError;

/// This macro obtains a connection to the database,
/// beware it must be in an async (sugar syntaxed) context.
#[macro_export]
macro_rules! db {
	() => {
		$database::utils::connection::get_db_connection().await?
	};
}

static CONNECTION: OnceLock<Pool<Postgres>> = OnceLock::new();

/// This obtains a database connection from the `CONNECTION` oncelock
/// or creates a new connection and stores it there.
///
/// This is not to be used directly, prefer `db!()` instead.
pub async fn get_db_connection<'r>(url: Option<&str>) -> Result<&'r Pool<Postgres>, DatabaseError> {
	if let Some(connection) = CONNECTION.get() {
		return Ok(connection);
	}

	let pool = PgPoolOptions::new() //
		.max_connections(5)
		.connect(url.unwrap_or(env!("DATABASE_URL")))
		.await?;

	sqlx::migrate!().run(&pool).await?;

	Ok(CONNECTION.get_or_init(|| pool))
}
