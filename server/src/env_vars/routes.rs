use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, EnvVarKey};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, EnvVarId, ProjectId, TeamId};
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

impl From<&EnvVarKey> for EnvVarResponse {
	fn from(e: &EnvVarKey) -> Self {
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
	let key = service::upsert(&s, user.id, team, project, app, &body.key, &body.value).await?;
	Ok(Json(EnvVarResponse::from(&key)))
}

/// Lists an application's env var keys (values redacted); caller must hold
/// `read env_var` on the team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<EnvVarResponse>>> {
	let keys = service::list(&s.db, user.id, team, project, app).await?;
	Ok(Json(keys.iter().map(EnvVarResponse::from).collect()))
}

/// Deletes an env var; caller must hold `delete env_var` on the team. Must
/// belong to the application named in the path.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, EnvVarId)>,
) -> AppResult<Json<DeleteEnvVarResponse>> {
	let id = service::remove(&s.db, user.id, team, project, app, id).await?;
	Ok(Json(DeleteEnvVarResponse { id }))
}
