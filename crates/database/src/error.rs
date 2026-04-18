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
}
