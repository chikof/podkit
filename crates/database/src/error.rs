use std::env::VarError;

use sqlx::Error as SqlxError;
use sqlx::migrate::MigrateError;
use thiserror::Error as ThisError;

/// Everything that can go wrong in this crate: query failures, migrations,
/// and env lookups all fold into one type so callers only need one `?`.
#[derive(ThisError, Debug)]
pub enum DatabaseError {
	/// Any sqlx query/connection error.
	#[error("{0:#}")]
	ConnectionError(#[from] SqlxError),

	/// Migration runner failed.
	#[error("{0:#}")]
	MigrateError(#[from] MigrateError),

	/// Catch-all for errors bubbled up from elsewhere via `anyhow`.
	#[error("{0}")]
	Anyhow(#[from] anyhow::Error),

	/// `DATABASE_URL` (or another expected env var) was missing or invalid.
	#[error("{0}")]
	EnvError(#[from] VarError),

	/// `migrate()` was called before the connection pool was initialized.
	#[error("Failed to run migrations")]
	MigrationError,
}
