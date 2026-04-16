use thiserror::Error as ThisError;

#[derive(ThisError, Debug)]
pub enum ServerError {
	#[error("std io: {0}")]
	StdIo(#[from] std::io::Error),

	#[error("axum: {0}")]
	Axum(#[from] axum::Error),
}
