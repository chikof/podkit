//! The JSON API of the importers, under `/api/v1/teams/{team_id}/importers`.
//!
//! - `POST /{source}/plan` with `{"url", "token", "server"}` reads the source and answers
//!   `{id, source, file, report, projects}`: the `podkit.toml` it made (no secret in it), what
//!   did not map, and the plan against the team. Nothing is made.
//! - `POST /{source}/apply` with `{"id"}` makes what a plan planned; with `{"url", "token",
//!   "server"}` instead it reads, plans and makes in one request. `"deploy": true` also
//!   queues a deploy of what is made.
//!
//! `source` is `coolify`, `dokploy` or `caprover`. The credential is read for the one request
//! and never stored or echoed.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use super::service::{self, Applied, Preview, Request};
use super::{Credentials, Source};
use crate::auth::AuthUser;
use crate::auth::tokens::Scope;
use crate::error::{AppResult, ServerError};
use crate::iac::service::ImportOutcome;
use crate::ids::TeamId;
use crate::state::AppState;

/// Under `/teams/{team_id}/importers`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/{source}/plan", post(plan))
		.route("/{source}/apply", post(apply))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
	#[serde(default)]
	url: String,
	#[serde(default)]
	token: String,
	#[serde(default)]
	server: String,
	/// Apply only: a preview made by `plan`.
	#[serde(default)]
	id: Option<String>,
	#[serde(default)]
	deploy: bool,
}

fn source(name: &str) -> AppResult<Source> {
	Source::parse(name).ok_or(ServerError::NotFound)
}

impl Body {
	fn request(self, source: Source) -> Request {
		Request {
			source,
			credentials: Credentials {
				url: self.url,
				token: self.token,
			},
			server: self.server,
		}
	}
}

fn previewed(preview: &Preview) -> serde_json::Value {
	json!({
		"id": preview.id,
		"source": preview.source,
		"file": preview.file,
		"report": preview.report,
		"projects": preview.plans,
	})
}

async fn plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, source_name)): Path<(TeamId, String)>,
	axum::Json(body): axum::Json<Body>,
) -> AppResult<Response> {
	let source = source(&source_name)?;
	let preview = service::preview(&s, user.id, team, &body.request(source)).await?;
	Ok(axum::Json(previewed(&preview)).into_response())
}

fn done(applied: &Applied, preview: Option<&Preview>) -> Response {
	let mut body = preview.map_or_else(|| json!({}), previewed);
	match &applied.outcome {
		ImportOutcome::Refused(plans) => {
			body["error"] = json!("the plan has issues; nothing was changed");
			body["projects"] = json!(plans);
			(StatusCode::UNPROCESSABLE_ENTITY, axum::Json(body)).into_response()
		}
		ImportOutcome::Done(made) => {
			let ok = made
				.iter()
				.all(|d| d.applied.report.ok() && !d.applied.plan.has_issues());
			body["applied"] = json!(made);
			let status = if ok {
				StatusCode::OK
			} else {
				StatusCode::CONFLICT
			};
			(status, axum::Json(body)).into_response()
		}
	}
}

async fn apply(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, source_name)): Path<(TeamId, String)>,
	axum::Json(body): axum::Json<Body>,
) -> AppResult<Response> {
	let source = source(&source_name)?;
	if body.deploy
		&& let Some(grant) = &user.grant
		&& !grant.allows(Scope::Deploy)
	{
		return Err(ServerError::MissingScope("deploy"));
	}
	let deploy = body.deploy;
	if let Some(id) = body.id.clone() {
		let applied = service::apply(&s, user.id, team, source, &id, deploy).await?;
		return Ok(done(&applied, None));
	}
	let (preview, applied) = service::run(&s, user.id, team, &body.request(source), deploy).await?;
	if let Some(applied) = applied {
		return Ok(done(&applied, Some(&preview)));
	}
	let mut out = previewed(&preview);
	out["error"] = json!(if preview.plans.is_empty() {
		"nothing to import; see the report"
	} else {
		"the plan has issues; nothing was changed"
	});
	Ok((StatusCode::UNPROCESSABLE_ENTITY, axum::Json(out)).into_response())
}
