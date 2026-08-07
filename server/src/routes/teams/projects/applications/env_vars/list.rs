use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::ApplicationId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

/// No `value` field: env var values are never returned after creation.
/// Redact-on-read applies the same way `webhook_secret`/deploy keys do:
/// the plaintext round-trips out exactly once, at write time.
#[derive(Serialize)]
pub struct EnvVarResponse {
	pub id: i64,
	pub application_id: i64,
	pub key: String,
}

/// Lists an application's env var keys (values redacted); caller must hold
/// `read env_var` on the team.
pub async fn list_env_vars(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, application_id)): Path<(i64, i64, i64)>,
) -> Result<Json<Vec<EnvVarResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"env_var",
	)
	.await?;

	let env_vars = state
		.env_vars
		.list_by_application(ApplicationId(application_id))
		.await?
		.into_iter()
		.map(|e| EnvVarResponse {
			id: e.id.0,
			application_id: e.application_id.0,
			key: e.key,
		})
		.collect();

	Ok(Json(env_vars))
}
