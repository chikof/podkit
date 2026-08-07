use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use podkit_core::domain::shared::ids::ApplicationId;
use serde::Deserialize;

use crate::{AppState, deploy_pipeline, error::ServerError};

pub fn router() -> Router<AppState> {
	Router::new().route("/deploy/{application_id}", post(deploy))
}

#[derive(Deserialize)]
pub struct DeployQuery {
	secret: Option<String>,
}

/// Provider-agnostic push-to-deploy webhook.
///
/// Not JWT-authenticated (external providers can't hold one); instead
/// authenticated by the per-application secret returned once at
/// application-creation time, passed as `X-Webhook-Secret` or `?secret=`.
/// Works with any webhook-capable system (GitHub/GitLab/Gitea generic
/// webhooks, a cron job, curl) without provider-specific signature parsing.
pub async fn deploy(
	State(state): State<AppState>,
	Path(application_id): Path<i64>,
	headers: HeaderMap,
	Query(query): Query<DeployQuery>,
) -> Result<StatusCode, ServerError> {
	let application = state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.ok_or(ServerError::ApplicationNotFound)?;

	// Unauthenticated requests must never reach the build pipeline, so
	// treat "no secret configured" the same as "wrong secret": forbidden.
	let Some(encrypted_secret) = application.webhook_secret.as_deref() else {
		return Err(ServerError::Forbidden);
	};
	let expected = state
		.secrets
		.decrypt(encrypted_secret)
		.map_err(|_| ServerError::Forbidden)?;

	let provided = headers
		.get("x-webhook-secret")
		.and_then(|v| v.to_str().ok())
		.map(str::to_string)
		.or(query.secret)
		.ok_or(ServerError::Forbidden)?;

	if !crypto::constant_time_eq(&provided, &expected) {
		return Err(ServerError::Forbidden);
	}

	deploy_pipeline::queue_and_spawn(state, application, None).await?;

	Ok(StatusCode::ACCEPTED)
}
