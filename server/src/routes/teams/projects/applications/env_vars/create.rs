use axum::Json;
use axum::extract::{Path, State};
use crypto::generate_id;
use podkit_core::domain::env_var::EnvVar;
use podkit_core::domain::shared::ids::{ApplicationId, EnvVarId, ProjectId};
use serde::{Deserialize, Serialize};

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct UpsertEnvVarRequest {
	pub key: String,
	pub value: String,
}

#[derive(Serialize)]
pub struct EnvVarResponse {
	pub id: i64,
	pub application_id: i64,
	pub key: String,
}

impl From<EnvVar> for EnvVarResponse {
	fn from(e: EnvVar) -> Self {
		Self {
			id: e.id.0,
			application_id: e.application_id.0,
			key: e.key,
		}
	}
}

/// Sets an application env var (create or update, same endpoint, keyed by
/// `key`). The value is never echoed back; see `list_env_vars` for why.
/// Caller must hold `create env_var` on the team.
pub async fn upsert_env_var(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id)): Path<(i64, i64, i64)>,
	Json(body): Json<UpsertEnvVarRequest>,
) -> Result<Json<EnvVarResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
		"env_var",
	)
	.await?;

	state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.filter(|a| a.project_id.0 == project_id)
		.ok_or(ServerError::ApplicationNotFound)?;

	let encrypted_value = state
		.secrets
		.encrypt(&body.value)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt value: {e}")))?;

	let env_var = EnvVar::new(
		EnvVarId(generate_id()),
		ApplicationId(application_id),
		body.key,
		encrypted_value,
	);
	let persisted = state.env_vars.upsert(&env_var).await?;

	Ok(Json(persisted.into()))
}
