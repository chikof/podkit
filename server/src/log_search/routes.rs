use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use super::service::{self, Answer, Query, SettingsInput, Source};
use super::{Export, Settings};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::TeamId;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/settings", get(get_settings).put(put_settings))
		.route("/sources", get(list_sources))
		.route("/search", post(search))
}

#[derive(Serialize)]
struct ExportResponse {
	kind: &'static str,
	url: String,
	tenant: Option<String>,
	/// Whether a header is saved. The header itself is never sent back.
	has_authorization: bool,
}

impl From<&Export> for ExportResponse {
	fn from(e: &Export) -> Self {
		Self {
			kind: e.kind.as_str(),
			url: e.url.clone(),
			tenant: e.tenant.clone(),
			has_authorization: e.has_authorization,
		}
	}
}

#[derive(Serialize)]
struct SettingsResponse {
	retention_days: i32,
	max_mb: i32,
	export: Option<ExportResponse>,
}

impl From<Settings> for SettingsResponse {
	fn from(s: Settings) -> Self {
		Self {
			retention_days: s.retention_days,
			max_mb: s.max_mb,
			export: s.export.as_ref().map(Into::into),
		}
	}
}

async fn get_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<SettingsResponse>> {
	Ok(Json(service::settings(&s.db, user.id, team).await?.into()))
}

async fn put_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(input): Json<SettingsInput>,
) -> AppResult<Json<SettingsResponse>> {
	Ok(Json(
		service::update_settings(&s, user.id, team, &input)
			.await?
			.into(),
	))
}

#[derive(Serialize)]
struct SourceResponse {
	service: String,
	kind: &'static str,
	name: String,
	project: String,
}

impl From<Source> for SourceResponse {
	fn from(s: Source) -> Self {
		Self {
			kind: s.kind.label(),
			service: s.service,
			name: s.name,
			project: s.project,
		}
	}
}

async fn list_sources(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<SourceResponse>>> {
	let sources = service::sources(&s.db, user.id, team).await?;
	Ok(Json(sources.into_iter().map(Into::into).collect()))
}

#[derive(Serialize)]
struct LineResponse {
	/// Unix milliseconds.
	ts: i64,
	service: String,
	name: String,
	server: i64,
	stream: proto::Stream,
	level: Option<&'static str>,
	deployment: Option<i64>,
	line: String,
}

#[derive(Serialize)]
struct SearchResponse {
	lines: Vec<LineResponse>,
	truncated: bool,
	partial: bool,
	/// Servers that did not answer; their lines are missing.
	unreachable: Vec<String>,
}

fn response(answer: &Answer) -> SearchResponse {
	SearchResponse {
		lines: answer
			.merged
			.lines
			.iter()
			.map(|l| LineResponse {
				ts: l.record.ts,
				name: answer.label(&l.record.svc),
				service: l.record.svc.clone(),
				server: l.server.0,
				stream: l.record.stream,
				level: l.record.level.map(proto::LogLevel::as_str),
				deployment: l.record.dep,
				line: l.record.line.clone(),
			})
			.collect(),
		truncated: answer.merged.truncated,
		partial: answer.merged.partial,
		unreachable: answer.unreachable.clone(),
	}
}

async fn search(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(query): Json<Query>,
) -> AppResult<Json<SearchResponse>> {
	let answer = service::search(&s, user.id, team, &query).await?;
	Ok(Json(response(&answer)))
}
