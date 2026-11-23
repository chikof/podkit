use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Serialize;
use serde_json::json;

use super::{delivery_id, header, is_authorized};
use crate::applications::SourceKind;
use crate::autodeploy::engine::{Outcome, Push};
use crate::error::{AppResult, ServerError};
use crate::ids::ApplicationId;
use crate::state::AppState;
use crate::{applications, autodeploy, deployments};

pub fn router() -> Router<AppState> {
	Router::new().route("/deploy/{application_id}", post(deploy))
}

#[derive(Serialize)]
struct Ignored {
	ignored: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	reason: Option<&'static str>,
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

	// A provider repeats a delivery it thinks failed; a captured request is replayed the same
	// way. Either way the second one deploys nothing. Recorded only after authentication.
	if let Some(delivery) = delivery_id(&headers)
		&& !crate::git_providers::first_delivery(&s.db, &format!("app:{application_id}:{delivery}"))
			.await?
	{
		return Ok(ignored(None));
	}

	// Pull request events and `/preview` comments: previews, not deploys.
	if let Some((kind, name)) = pull_request_event(&headers) {
		let Some(event) = crate::previews::event::parse(kind, name, &body) else {
			return Ok(ignored(None));
		};
		return Ok(
			match crate::previews::service::handle(&s, &application, &event).await? {
				crate::previews::service::Outcome::Ignored => ignored(Some("previews")),
				_ => (StatusCode::ACCEPTED, Json(json!({ "preview": true }))).into_response(),
			},
		);
	}

	let not_a_push = header(&headers, "x-github-event").is_some_and(|event| event != "push")
		|| header(&headers, "x-gitea-event").is_some_and(|event| event != "push")
		|| header(&headers, "x-gitlab-event").is_some_and(|event| event != "Push Hook");
	if not_a_push {
		return Ok(ignored(None));
	}

	// A git application goes through autodeploy: the branch it follows (or its patterns),
	// path filters, the commit's checks, freezes and skip markers. An image application is
	// told to redeploy by whatever calls the URL.
	if application.source_kind == SourceKind::Git {
		let push = Push::from_payload(&body);
		return Ok(
			match autodeploy::engine::handle(&s, &application, &push).await? {
				Outcome::Deployed(d) => {
					(StatusCode::ACCEPTED, Json(json!({ "deployment": d.id.0 }))).into_response()
				}
				Outcome::Waiting => held("waiting_ci"),
				Outcome::Frozen => held("frozen"),
				Outcome::Skipped => ignored(Some("skipped")),
				Outcome::Filtered => ignored(Some("filtered")),
				Outcome::Failed => ignored(Some("checks")),
				Outcome::Ignored => ignored(None),
			},
		);
	}

	deployments::queue(&s, &application, None, None).await?;
	Ok(StatusCode::ACCEPTED.into_response())
}

fn held(state: &'static str) -> Response {
	(StatusCode::ACCEPTED, Json(json!({ "held": state }))).into_response()
}

fn ignored(reason: Option<&'static str>) -> Response {
	Json(Ignored {
		ignored: true,
		reason,
	})
	.into_response()
}

/// Who sent the delivery and what event it names, when it is one previews follow: a pull (or
/// merge) request, or a comment. Gitea also sends GitHub's headers, so its own come first.
fn pull_request_event(headers: &HeaderMap) -> Option<(crate::git_providers::Kind, &str)> {
	use crate::git_providers::Kind;
	if let Some(e) = header(headers, "x-gitlab-event") {
		return matches!(e, "Merge Request Hook" | "Note Hook").then_some((Kind::Gitlab, e));
	}
	if let Some(e) = header(headers, "x-gitea-event") {
		return matches!(e, "pull_request" | "issue_comment").then_some((Kind::Gitea, e));
	}
	let e = header(headers, "x-github-event")?;
	matches!(e, "pull_request" | "issue_comment").then_some((Kind::GithubApp, e))
}
