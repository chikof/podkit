//! HTTP handlers of image auto-update, under
//! `/api/v1/teams/{team}/projects/{project}/applications/{app}/auto-update`. They only
//! translate: the rules live in [`super::service`].

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::rules::{self, Window};
use super::service::{self, SettingsInput, View};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(show).put(update))
		.route("/check", post(check))
}

type Params = Path<(TeamId, ProjectId, ApplicationId)>;

#[derive(Deserialize, Serialize)]
struct WindowBody {
	/// `HH:MM`.
	start: String,
	end: String,
	#[serde(default)]
	utc_offset_minutes: i64,
}

#[derive(Deserialize)]
struct UpdateRequest {
	enabled: bool,
	/// `hourly`, `6h` or `daily`.
	#[serde(default)]
	interval: Option<String>,
	/// `null` or absent: no window.
	#[serde(default)]
	window: Option<WindowBody>,
}

#[derive(Serialize)]
struct AutoUpdateResponse {
	enabled: bool,
	interval: &'static str,
	window: Option<WindowBody>,
	image: Option<String>,
	/// Why the application cannot be auto-updated, when it cannot.
	unavailable: Option<&'static str>,
	frozen_now: bool,
	next_check_at: Option<String>,
	last_checked_at: Option<String>,
	last_result: Option<&'static str>,
	last_detail: String,
	remote_digest: Option<String>,
	/// The newest deployment auto-update started, and the digest it was for.
	deployment_id: Option<i64>,
	deployed_digest: Option<String>,
	/// Held back after its auto-update failed or was rolled back, until the tag moves.
	bad_digest: Option<String>,
}

fn stamp(at: Option<OffsetDateTime>) -> Option<String> {
	at.and_then(|t| {
		t.format(&time::format_description::well_known::Rfc3339)
			.ok()
	})
}

fn window_body(w: Window) -> WindowBody {
	WindowBody {
		start: rules::time_label(w.start),
		end: rules::time_label(w.end),
		utc_offset_minutes: w.offset,
	}
}

fn response(v: &View) -> AutoUpdateResponse {
	let settings = v.settings();
	let row = v.row.as_ref();
	AutoUpdateResponse {
		enabled: settings.enabled,
		interval: settings.interval.as_str(),
		window: settings.window.map(window_body),
		image: v.image.clone(),
		unavailable: v.unavailable,
		frozen_now: v.frozen_now,
		next_check_at: stamp(row.and_then(|r| r.next_check_at)),
		last_checked_at: stamp(row.and_then(|r| r.last_checked_at)),
		last_result: row.and_then(super::Row::last).map(rules::Checked::as_str),
		last_detail: row.map(|r| r.last_detail.clone()).unwrap_or_default(),
		remote_digest: row.and_then(|r| r.remote_digest.clone()),
		deployment_id: row.and_then(|r| r.deployment_id),
		deployed_digest: row.and_then(|r| r.deployed_digest.clone()),
		bad_digest: row.and_then(|r| r.bad_digest.clone()),
	}
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Params,
) -> AppResult<Json<AutoUpdateResponse>> {
	let view = service::get(&s, user.id, team, project, app).await?;
	Ok(Json(response(&view)))
}

async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Params,
	Json(body): Json<UpdateRequest>,
) -> AppResult<Json<AutoUpdateResponse>> {
	let input = SettingsInput {
		enabled: body.enabled,
		interval: body.interval.unwrap_or_default(),
		window_start: body
			.window
			.as_ref()
			.map(|w| w.start.clone())
			.unwrap_or_default(),
		window_end: body
			.window
			.as_ref()
			.map(|w| w.end.clone())
			.unwrap_or_default(),
		offset: body
			.window
			.as_ref()
			.map(|w| w.utc_offset_minutes.to_string())
			.unwrap_or_default(),
	};
	let view = service::save(&s, user.id, team, project, app, &input).await?;
	Ok(Json(response(&view)))
}

async fn check(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Params,
) -> AppResult<Json<AutoUpdateResponse>> {
	let view = service::check_now(&s, user.id, team, project, app).await?;
	Ok(Json(response(&view)))
}
