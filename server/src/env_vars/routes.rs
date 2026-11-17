use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::EnvVar;
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvVarId, ProjectId, TeamId};
use crate::scope;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(upsert).get(list))
		.route("/{env_var_id}", delete(remove))
}

#[derive(Deserialize)]
pub struct UpsertEnvVarRequest {
	pub key: String,
	pub value: String,
}

/// No `value` field: env var values are never returned after creation.
/// Redact-on-read applies the same way `webhook_secret`/deploy keys do:
/// the plaintext round-trips out exactly once, at write time.
#[derive(Serialize)]
pub struct EnvVarResponse {
	pub id: EnvVarId,
	pub application_id: ApplicationId,
	pub key: String,
}

impl From<&EnvVar> for EnvVarResponse {
	fn from(e: &EnvVar) -> Self {
		Self {
			id: e.id,
			application_id: e.application_id,
			key: e.key.clone(),
		}
	}
}

#[derive(Serialize)]
pub struct DeleteEnvVarResponse {
	pub id: EnvVarId,
}

/// Sets an application env var (create or update, same endpoint, keyed by
/// `key`). The value is never echoed back. Caller must hold `create env_var`
/// on the team.
pub async fn upsert(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<UpsertEnvVarRequest>,
) -> AppResult<Json<EnvVarResponse>> {
	authorize(&s.db, user.id, team, Action::Create, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	let encrypted = s
		.secrets
		.encrypt(&body.value)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt value: {e}")))?;
	let env_var = super::upsert(&s.db, app.id, &body.key, &encrypted).await?;

	Ok(Json(EnvVarResponse::from(&env_var)))
}

/// Lists an application's env var keys (values redacted); caller must hold
/// `read env_var` on the team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<EnvVarResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	let env_vars = super::list(&s.db, app.id).await?;
	Ok(Json(env_vars.iter().map(EnvVarResponse::from).collect()))
}

/// Deletes an env var; caller must hold `delete env_var` on the team. Must
/// belong to the application named in the path.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, EnvVarId)>,
) -> AppResult<Json<DeleteEnvVarResponse>> {
	authorize(&s.db, user.id, team, Action::Delete, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	if !super::delete(&s.db, app.id, id).await? {
		return Err(ServerError::EnvVarNotFound);
	}
	Ok(Json(DeleteEnvVarResponse { id }))
}
