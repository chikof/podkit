//! GitHub's push webhook: one URL per GitHub App, signed with the app's webhook secret. A push
//! to a branch deploys the applications that clone that repository and branch through the
//! app.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use super::{Credentials, applications_for_push, first_delivery, service};
use crate::autodeploy::engine::{self, Outcome};
use crate::error::{AppResult, ServerError};
use crate::ids::GitProviderId;
use crate::state::AppState;
use crate::webhooks::{header, verify_github_signature};

#[derive(Deserialize)]
struct Push {
	#[serde(rename = "ref", default)]
	git_ref: String,
	#[serde(default)]
	deleted: bool,
	repository: Repository,
}

#[derive(Deserialize)]
struct Repository {
	full_name: String,
}

/// The branch a push ref names, if it is a branch.
pub(crate) fn branch_of(git_ref: &str) -> Option<&str> {
	git_ref
		.strip_prefix("refs/heads/")
		.filter(|b| !b.is_empty())
}

/// `POST /git-providers/github/webhook/{provider_id}`.
///
/// Not session-authenticated: GitHub holds no session. The request is authentic when its
/// `X-Hub-Signature-256` is the HMAC of the body under the app's webhook secret, compared in
/// constant time. A provider that does not exist, has no secret, or gets a bad signature all
/// answer the same `403`. Only then is the delivery id recorded, so a captured request
/// replayed later is acknowledged and ignored.
pub(super) async fn github(
	State(s): State<AppState>,
	Path(id): Path<GitProviderId>,
	headers: HeaderMap,
	body: Bytes,
) -> AppResult<Response> {
	let provider = service::for_webhook(&s.db, id)
		.await
		.map_err(|_| ServerError::Forbidden)?;
	let Ok((_, Credentials::GithubApp { webhook_secret, .. })) = service::open(&s, &provider)
	else {
		return Err(ServerError::Forbidden);
	};
	let signed = !webhook_secret.is_empty()
		&& header(&headers, "x-hub-signature-256")
			.is_some_and(|sig| verify_github_signature(&webhook_secret, &body, sig));
	if !signed {
		return Err(ServerError::Forbidden);
	}

	if let Some(delivery) = header(&headers, "x-github-delivery")
		&& !first_delivery(&s.db, &format!("github:{id}:{delivery}")).await?
	{
		return Ok(Json(json!({ "ignored": "replayed" })).into_response());
	}

	match header(&headers, "x-github-event") {
		Some("ping") => Ok(Json(json!({ "pong": true })).into_response()),
		Some("push") => {
			let Ok(push) = serde_json::from_slice::<Push>(&body) else {
				return Err(ServerError::Validation("not a push event".to_owned()));
			};
			let tag = push.git_ref.starts_with("refs/tags/");
			if (branch_of(&push.git_ref).is_none() && !tag) || push.deleted {
				return Ok(Json(json!({ "ignored": "not a branch push" })).into_response());
			}
			let event = engine::Push::from_payload(&body);
			let (mut queued, mut held) = (0, 0);
			for app in applications_for_push(&s.db, id, &push.repository.full_name).await? {
				if let Some(app) = crate::applications::find_by_id(&s.db, app).await? {
					match engine::handle(&s, &app, &event).await? {
						Outcome::Deployed(_) => queued += 1,
						Outcome::Waiting | Outcome::Frozen => held += 1,
						_ => {}
					}
				}
			}
			// A tag that no application deploys is not a push anything follows.
			if tag && queued + held == 0 {
				return Ok(Json(json!({ "ignored": "not a branch push" })).into_response());
			}
			Ok((
				StatusCode::ACCEPTED,
				Json(json!({ "queued": queued, "held": held })),
			)
				.into_response())
		}
		Some(event @ ("pull_request" | "issue_comment")) => {
			let handled = crate::previews::service::handle_for_repo(
				&s,
				id,
				crate::git_providers::Kind::GithubApp,
				event,
				&body,
			)
			.await?;
			if handled == 0 {
				return Ok(Json(json!({ "ignored": "previews" })).into_response());
			}
			Ok((StatusCode::ACCEPTED, Json(json!({ "previews": handled }))).into_response())
		}
		_ => Ok(Json(json!({ "ignored": "event" })).into_response()),
	}
}
