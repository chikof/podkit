use std::env::VarError;

use sqlx::migrate::MigrateError;
use sqlx::Error as SqlxError;
use thiserror::Error as ThisError;

#[derive(ThisError, Debug)]
pub enum DatabaseError {
	#[error("{0:#}")]
	ConnectionError(#[from] SqlxError),

	#[error("{0:#}")]
	MigrateError(#[from] MigrateError),

	#[error("{0}")]
	Anyhow(#[from] anyhow::Error),

	#[error("{0}")]
	EnvError(#[from] VarError),

	#[error("Failed to run migrations")]
	MigrationError,
}
