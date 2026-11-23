//! The JSON API of private access. The web UI uses the same service functions.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, SettingsInput};
use super::{Exposure, Settings};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;

/// `/teams/{team_id}/tailscale`: the team's settings.
pub fn router() -> Router<AppState> {
	Router::new().route("/", get(get_settings).put(put_settings))
}

/// `…/applications/{app_id}/tailscale`: the application's exposure.
pub fn application_router() -> Router<AppState> {
	Router::new().route("/", get(show).put(enable).delete(disable))
}

/// The settings as the API shows them: the auth key is never in them.
#[derive(Serialize)]
struct SettingsResponse {
	auth_key_set: bool,
	login_server: Option<String>,
	tags: Vec<String>,
}

impl From<Settings> for SettingsResponse {
	fn from(s: Settings) -> Self {
		Self {
			auth_key_set: s.has_auth_key,
			login_server: s.login_server,
			tags: s.tags,
		}
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SettingsRequest {
	/// Write-only. Absent or empty keeps the stored key.
	auth_key: String,
	/// Forgets the stored key.
	clear_auth_key: bool,
	/// A Headscale URL; empty joins Tailscale itself.
	login_server: String,
	/// `tag:name`, separated by commas or spaces.
	tags: String,
}

/// The team's settings; caller must hold `read team`.
async fn get_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<SettingsResponse>> {
	Ok(Json(service::settings(&s, user.id, team).await?.into()))
}

/// Replaces the settings; caller must hold `update team`.
async fn put_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<SettingsRequest>,
) -> AppResult<Json<SettingsResponse>> {
	let saved = service::save_settings(
		&s,
		user.id,
		team,
		&SettingsInput {
			auth_key: &body.auth_key,
			clear_auth_key: body.clear_auth_key,
			login_server: &body.login_server,
			tags: &body.tags,
		},
	)
	.await?;
	Ok(Json(saved.into()))
}

#[derive(Serialize)]
struct ExposureResponse {
	hostname: String,
	status: &'static str,
	/// The address on the tailnet, once the node has joined.
	url: Option<String>,
	error: Option<String>,
}

impl From<&Exposure> for ExposureResponse {
	fn from(e: &Exposure) -> Self {
		Self {
			hostname: e.hostname.clone(),
			status: e.status.as_str(),
			url: e.url(),
			error: e.error.clone(),
		}
	}
}

#[derive(Serialize)]
struct ShowResponse {
	/// `null` while the application is not on the tailnet.
	exposure: Option<ExposureResponse>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EnableRequest {
	/// The node's name; the application's slug when empty.
	hostname: String,
}

type AppPath = Path<(TeamId, ProjectId, ApplicationId)>;

/// The application's exposure; caller must hold `read domain`.
async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): AppPath,
) -> AppResult<Json<ShowResponse>> {
	let view = service::view(&s, user.id, team, project, app).await?;
	Ok(Json(ShowResponse {
		exposure: view.exposure.as_ref().map(Into::into),
	}))
}

/// Starts serving the application on the tailnet; caller must hold `create domain`. The
/// sidecar starts in a job: poll `GET` until `status` is `running`.
async fn enable(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): AppPath,
	body: Option<Json<EnableRequest>>,
) -> AppResult<Json<ExposureResponse>> {
	let hostname = body.map(|Json(b)| b.hostname).unwrap_or_default();
	let exposure = service::enable(&s, user.id, team, project, app, &hostname).await?;
	Ok(Json((&exposure).into()))
}

/// Stops serving it: the node is logged out and the sidecar removed; caller must hold
/// `delete domain`.
async fn disable(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): AppPath,
) -> AppResult<Json<ShowResponse>> {
	let exposure = service::disable(&s, user.id, team, project, app).await?;
	Ok(Json(ShowResponse {
		exposure: exposure.as_ref().map(Into::into),
	}))
}
