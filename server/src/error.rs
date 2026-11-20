use axum::{
	Json,
	http::StatusCode,
	response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error as ThisError;

#[derive(ThisError, Debug)]
pub enum ServerError {
	#[error("std io: {0}")]
	StdIo(#[from] std::io::Error),

	#[error("axum: {0}")]
	Axum(#[from] axum::Error),

	#[error("forgeconf: {0}")]
	Forgeconf(#[from] forgeconf::ConfigError),

	#[error("db: {0}")]
	Sqlx(#[from] sqlx::Error),

	#[error("You forgot the include the token buddy")]
	MissingToken,

	#[error("Wrong token buddy")]
	InvalidToken,

	#[error("Invalid credentials")]
	InvalidCredentials,

	#[error("{0}")]
	Validation(String),

	#[error("an account with this email already exists")]
	EmailTaken,

	#[error("user is already a member of this team")]
	AlreadyMember,

	#[error("that role does not belong to this team")]
	RoleNotAllowed,

	#[error("{0}")]
	Conflict(String),

	#[error("registration is closed")]
	RegistrationClosed,

	#[error("environment not found")]
	EnvironmentNotFound,

	#[error("project not found")]
	ProjectNotFound,

	/// A page or resource that is not there or not visible to the caller;
	/// used by the web UI for paths that name no specific resource.
	#[error("not found")]
	NotFound,

	#[error("server not found")]
	ServerNotFound,

	#[error("application not found")]
	ApplicationNotFound,

	#[error("deployment not found")]
	DeploymentNotFound,

	#[error("env var not found")]
	EnvVarNotFound,

	#[error("domain not found")]
	DomainNotFound,

	#[error("database not found")]
	DatabaseNotFound,

	#[error("volume not found")]
	VolumeNotFound,

	#[error("backup not found")]
	BackupNotFound,

	#[error("backup target not found")]
	BackupTargetNotFound,

	#[error("model file not found")]
	ModelFileNotFound,

	#[error("model not found")]
	ModelNotFound,

	#[error("stack not found")]
	StackNotFound,

	#[error("ssh key not found")]
	SshKeyNotFound,

	#[error("git provider not found")]
	GitProviderNotFound,

	/// A git provider refused or could not be reached. The message is written for the person
	/// who connected it and never carries a credential.
	#[error("{0}")]
	GitProvider(String),

	/// The request is well-formed but names something podkit does not run,
	/// e.g. a compose key outside the supported subset. `issues` is shown to
	/// the caller next to the message.
	#[error("{message}")]
	Unprocessable {
		message: String,
		issues: serde_json::Value,
	},

	#[error("could not reach Hugging Face")]
	HuggingFaceUnavailable,

	#[error("that hostname is already routed to another application")]
	DomainTaken,

	#[error("that slug is already used by another application on this server")]
	ApplicationSlugTaken,

	#[error("the local server cannot be deleted")]
	CannotDeleteLocalServer,

	#[error("invalid AGE_SECRET_KEY: {0}")]
	InvalidSecretKey(String),

	#[error("you don't have permission to do that")]
	Forbidden,

	#[error("internal error")]
	Internal,

	#[error("server agent unavailable")]
	AgentUnavailable,

	#[error("server agent is not installed yet; bootstrap the server first")]
	ServerNotReady,
}

impl ServerError {
	pub fn status(&self) -> StatusCode {
		match self {
			Self::StdIo(_)
			| Self::Axum(_)
			| Self::Forgeconf(_)
			| Self::Sqlx(_)
			| Self::InvalidSecretKey(_)
			| Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
			Self::MissingToken
			| Self::Validation(_)
			| Self::RoleNotAllowed
			| Self::CannotDeleteLocalServer => StatusCode::BAD_REQUEST,
			Self::InvalidToken | Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
			Self::EmailTaken
			| Self::AlreadyMember
			| Self::DomainTaken
			| Self::ApplicationSlugTaken
			| Self::Conflict(_)
			| Self::ServerNotReady => StatusCode::CONFLICT,
			Self::ProjectNotFound
			| Self::EnvironmentNotFound
			| Self::NotFound
			| Self::ServerNotFound
			| Self::ApplicationNotFound
			| Self::DeploymentNotFound
			| Self::EnvVarNotFound
			| Self::DomainNotFound
			| Self::DatabaseNotFound
			| Self::VolumeNotFound
			| Self::BackupNotFound
			| Self::BackupTargetNotFound
			| Self::ModelFileNotFound
			| Self::StackNotFound
			| Self::SshKeyNotFound
			| Self::GitProviderNotFound
			| Self::ModelNotFound => StatusCode::NOT_FOUND,
			Self::Unprocessable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
			Self::Forbidden | Self::RegistrationClosed => StatusCode::FORBIDDEN,
			Self::AgentUnavailable | Self::HuggingFaceUnavailable | Self::GitProvider(_) => {
				StatusCode::BAD_GATEWAY
			}
		}
	}
}

#[derive(Serialize)]
struct ErrorBody<'a> {
	error: &'a str,
}

#[derive(Serialize)]
struct IssuesBody<'a> {
	error: &'a str,
	issues: &'a serde_json::Value,
}

impl IntoResponse for ServerError {
	fn into_response(self) -> Response {
		let status = self.status();
		if status == StatusCode::INTERNAL_SERVER_ERROR {
			tracing::error!(error = %self, "request failed");
			return (
				status,
				Json(ErrorBody {
					error: "internal server error",
				}),
			)
				.into_response();
		}
		let message = self.to_string();
		if let Self::Unprocessable { issues, .. } = &self {
			return (
				status,
				Json(IssuesBody {
					error: &message,
					issues,
				}),
			)
				.into_response();
		}
		(status, Json(ErrorBody { error: &message })).into_response()
	}
}

pub type AppResult<T> = Result<T, ServerError>;

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn internal_errors_do_not_leak_details() {
		let resp = ServerError::StdIo(std::io::Error::other("secret path /x")).into_response();
		assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
		let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
			.await
			.unwrap();
		assert_eq!(&body[..], br#"{"error":"internal server error"}"#);
	}

	#[tokio::test]
	async fn client_errors_keep_their_message() {
		let resp = ServerError::Forbidden.into_response();
		assert_eq!(resp.status(), StatusCode::FORBIDDEN);
		let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
			.await
			.unwrap();
		assert_eq!(
			&body[..],
			br#"{"error":"you don't have permission to do that"}"#
		);
	}
}
