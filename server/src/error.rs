use axum::http::StatusCode;
use axum_thiserror::ErrorStatus;
use thiserror::Error as ThisError;

#[derive(ThisError, Debug, ErrorStatus)]
pub enum ServerError {
	#[error("std io: {0}")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	StdIo(#[from] std::io::Error),

	#[error("axum: {0}")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	Axum(#[from] axum::Error),

	#[error("forgeconf: {0}")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	Forgeconf(#[from] forgeconf::ConfigError),

	#[error("db: {0}")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	Database(#[from] database::DatabaseError),

	#[error("You forgot the include the token buddy")]
	#[status(StatusCode::BAD_REQUEST)]
	MissingToken,

	#[error("Wrong token buddy")]
	#[status(StatusCode::UNAUTHORIZED)]
	InvalidToken,

	#[error("Invalid credentials")]
	#[status(StatusCode::UNAUTHORIZED)]
	InvalidCredentials,

	#[error("internal error")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	Internal,
}

pub type AppResult<T> = Result<T, ServerError>;
