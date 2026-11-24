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
		.route("/bulk", post(bulk))
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

#[derive(Deserialize)]
pub struct BulkEnvVarRequest {
	/// `.env` text: `KEY=value` lines, `#` comments, optional `export ` and quotes.
	pub text: String,
}

/// A line of the text that was skipped: its number and what is wrong, never its content.
#[derive(Serialize)]
pub struct BulkLineError {
	pub line: usize,
	pub message: &'static str,
}

#[derive(Serialize)]
pub struct BulkEnvVarResponse {
	/// How many variables were set (created or replaced).
	pub saved: usize,
	pub errors: Vec<BulkLineError>,
}

/// Sets every good line of a `.env` text at once, in one transaction, and names the lines
/// it skipped. Variables not in the text are left alone. Same rules as the web UI's paste
/// box; caller must hold `create env_var` on the team. No value is echoed back.
pub async fn bulk(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<BulkEnvVarRequest>,
) -> AppResult<Json<BulkEnvVarResponse>> {
	let report = service::bulk_set(&s, user.id, team, project, app, &body.text).await?;
	Ok(Json(BulkEnvVarResponse {
		saved: report.saved,
		errors: report
			.errors
			.iter()
			.map(|e| BulkLineError {
				line: e.line,
				message: e.message,
			})
			.collect(),
	}))
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
