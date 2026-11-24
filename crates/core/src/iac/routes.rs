//! The JSON API of infrastructure as code. A file is sent as the request body (TOML), or as
//! JSON `{ "file": "...", "secrets": { "NAME": "value" }, "prune": false, "deploy": false }`
//! when secrets go with it.

use std::collections::BTreeMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

use super::objects::Secrets;
use super::service::{self, ImportOutcome, Input, Outcome};
use crate::auth::AuthUser;
use crate::auth::tokens::Scope;
use crate::error::{AppResult, ServerError};
use crate::ids::{ProjectId, TeamId};
use crate::state::AppState;

/// Under `/teams/{team_id}/projects/{project_id}`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/export", get(export))
		.route("/plan", post(plan))
		.route("/apply", post(apply))
}

/// Under `/teams/{team_id}/import`.
pub fn import_router() -> Router<AppState> {
	Router::new()
		.route("/plan", post(import_plan))
		.route("/apply", post(import_apply))
}

#[derive(Deserialize, Default)]
struct Flags {
	#[serde(default)]
	prune: Option<bool>,
	#[serde(default)]
	deploy: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
	file: String,
	#[serde(default)]
	secrets: BTreeMap<String, String>,
	#[serde(default)]
	prune: bool,
	#[serde(default)]
	deploy: bool,
}

fn is_json(headers: &HeaderMap) -> bool {
	headers
		.get(header::CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.is_some_and(|v| v.starts_with("application/json"))
}

/// Reads the request body: JSON with secrets, or the file itself.
fn input(headers: &HeaderMap, flags: &Flags, body: &Bytes) -> AppResult<Input> {
	if is_json(headers) {
		let envelope: Envelope = serde_json::from_slice(body)
			.map_err(|e| ServerError::Validation(format!("the request is not valid: {e}")))?;
		return Ok(Input {
			text: envelope.file,
			secrets: Secrets(envelope.secrets),
			prune: flags.prune.unwrap_or(envelope.prune),
			deploy: flags.deploy.unwrap_or(envelope.deploy),
		});
	}
	let text = std::str::from_utf8(body)
		.map_err(|_| ServerError::Validation("the file is not UTF-8 text".to_owned()))?
		.to_owned();
	Ok(Input {
		text,
		secrets: Secrets::default(),
		prune: flags.prune.unwrap_or(false),
		deploy: flags.deploy.unwrap_or(false),
	})
}

/// A token needs `delete` to prune and `deploy` to deploy, on top of `write` to apply.
fn scopes(user: &AuthUser, input: &Input) -> AppResult<()> {
	let Some(grant) = &user.grant else {
		return Ok(());
	};
	if input.prune && !grant.allows(Scope::Delete) {
		return Err(ServerError::MissingScope("delete"));
	}
	if input.deploy && !grant.allows(Scope::Deploy) {
		return Err(ServerError::MissingScope("deploy"));
	}
	Ok(())
}

async fn export(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Response> {
	let text = service::export(&s, user.id, team, project).await?;
	Ok((
		[
			(header::CONTENT_TYPE, "application/toml; charset=utf-8"),
			(
				header::CONTENT_DISPOSITION,
				"attachment; filename=\"podkit.toml\"",
			),
		],
		text,
	)
		.into_response())
}

async fn plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(flags): Query<Flags>,
	headers: HeaderMap,
	body: Bytes,
) -> AppResult<Response> {
	let input = input(&headers, &flags, &body)?;
	let plan = service::plan(&s, user.id, team, project, &input).await?;
	Ok(axum::Json(plan).into_response())
}

fn applied(outcome: Outcome) -> Response {
	match outcome {
		Outcome::Refused(plan) => (
			StatusCode::UNPROCESSABLE_ENTITY,
			axum::Json(
				json!({ "error": "the plan has issues; nothing was changed", "plan": plan }),
			),
		)
			.into_response(),
		Outcome::Done(done) => {
			let status = if done.report.ok() {
				StatusCode::OK
			} else {
				StatusCode::CONFLICT
			};
			(status, axum::Json(*done)).into_response()
		}
	}
}

async fn apply(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(flags): Query<Flags>,
	headers: HeaderMap,
	body: Bytes,
) -> AppResult<Response> {
	let input = input(&headers, &flags, &body)?;
	scopes(&user, &input)?;
	let outcome = service::apply(&s, user.id, team, project, &input).await?;
	Ok(applied(outcome))
}

async fn import_plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(flags): Query<Flags>,
	headers: HeaderMap,
	body: Bytes,
) -> AppResult<Response> {
	let input = input(&headers, &flags, &body)?;
	let plans = service::import_plan(&s, user.id, team, &input).await?;
	Ok(axum::Json(json!({ "projects": plans })).into_response())
}

async fn import_apply(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(flags): Query<Flags>,
	headers: HeaderMap,
	body: Bytes,
) -> AppResult<Response> {
	let input = input(&headers, &flags, &body)?;
	scopes(&user, &input)?;
	Ok(
		match service::import_apply(&s, user.id, team, &input).await? {
			ImportOutcome::Refused(plans) => (
				StatusCode::UNPROCESSABLE_ENTITY,
				axum::Json(json!({
					"error": "the plan has issues; nothing was changed",
					"projects": plans,
				})),
			)
				.into_response(),
			ImportOutcome::Done(done) => {
				let ok = done
					.iter()
					.all(|d| d.applied.report.ok() && !d.applied.plan.has_issues());
				let status = if ok {
					StatusCode::OK
				} else {
					StatusCode::CONFLICT
				};
				(status, axum::Json(json!({ "projects": done }))).into_response()
			}
		},
	)
}
