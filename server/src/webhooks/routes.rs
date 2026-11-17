use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Serialize;

use super::{header, is_authorized};
use crate::error::{AppResult, ServerError};
use crate::ids::ApplicationId;
use crate::state::AppState;
use crate::{applications, deploy_pipeline};

pub fn router() -> Router<AppState> {
	Router::new().route("/deploy/{application_id}", post(deploy))
}

#[derive(Serialize)]
struct Ignored {
	ignored: bool,
}

/// Provider-agnostic push-to-deploy webhook.
///
/// Not session-authenticated (external providers can't hold one); instead
/// authenticated by the per-application secret returned once at
/// application-creation time, presented as a GitHub `X-Hub-Signature-256`
/// HMAC over the raw body, an `X-Gitlab-Token`, or an `X-Webhook-Secret`.
/// The secret is never accepted in the query string, where it would leak
/// into proxy logs. A GitHub event other than `push` (e.g. `ping`) is
/// acknowledged without deploying; a push to any branch redeploys `git_ref`.
async fn deploy(
	State(s): State<AppState>,
	Path(application_id): Path<ApplicationId>,
	headers: HeaderMap,
	body: Bytes,
) -> AppResult<Response> {
	// The one place an application is loaded without a team: the caller is
	// an external system and the webhook secret is the credential.
	let application = applications::find_by_id(&s.db, application_id)
		.await?
		.ok_or(ServerError::ApplicationNotFound)?;

	// Unauthenticated requests must never reach the build pipeline, so
	// treat "no secret configured" the same as "wrong secret": forbidden.
	let Some(encrypted_secret) = application.webhook_secret.as_deref() else {
		return Err(ServerError::Forbidden);
	};
	let secret = s
		.secrets
		.decrypt(encrypted_secret)
		.map_err(|_| ServerError::Forbidden)?;

	if !is_authorized(&headers, &body, &secret) {
		return Err(ServerError::Forbidden);
	}

	if header(&headers, "x-github-event").is_some_and(|event| event != "push") {
		return Ok(Json(Ignored { ignored: true }).into_response());
	}

	deploy_pipeline::queue_and_spawn(&s, application, None).await?;
	Ok(StatusCode::ACCEPTED.into_response())
}
