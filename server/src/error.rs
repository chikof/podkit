use axum::http::StatusCode;
use macros::JsonError;
use podkit_core::domain::shared::errors::DomainError;
use thiserror::Error as ThisError;

#[derive(ThisError, Debug, JsonError)]
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

	#[error("domain: {0}")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	Domain(#[from] DomainError),

	#[error("You forgot the include the token buddy")]
	#[status(StatusCode::BAD_REQUEST)]
	MissingToken,

	#[error("Wrong token buddy")]
	#[status(StatusCode::UNAUTHORIZED)]
	InvalidToken,

	#[error("Invalid credentials")]
	#[status(StatusCode::UNAUTHORIZED)]
	InvalidCredentials,

	#[error("{0}")]
	#[status(StatusCode::BAD_REQUEST)]
	Validation(String),

	#[error("an account with this email already exists")]
	#[status(StatusCode::CONFLICT)]
	EmailTaken,

	#[error("user is already a member of this team")]
	#[status(StatusCode::CONFLICT)]
	AlreadyMember,

	#[error("that role does not belong to this team")]
	#[status(StatusCode::BAD_REQUEST)]
	RoleNotAllowed,

	#[error("you don't have permission to do that")]
	#[status(StatusCode::FORBIDDEN)]
	Forbidden,

	#[error("internal error")]
	#[status(StatusCode::INTERNAL_SERVER_ERROR)]
	Internal,
}

pub type AppResult<T> = Result<T, ServerError>;
