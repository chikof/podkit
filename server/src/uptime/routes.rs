use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::service::{self, CheckInput, Incident};
use super::{Check, DayStat, LatencyPoint, uptime_percent};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UptimeCheckId};
use crate::metrics::Range;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/checks", get(list).post(create))
		.route("/checks/{check_id}", get(show).put(update).delete(remove))
		.route("/checks/{check_id}/run", post(run))
		.route("/checks/{check_id}/results", get(results))
		.route("/checks/{check_id}/incidents", get(incidents))
		.route("/status-page", get(get_status_page).put(put_status_page))
		.route("/settings", get(get_settings).put(put_settings))
}

/// A check as the API shows it. The target is in here: only members of the team read it.
#[derive(Serialize)]
struct CheckResponse {
	id: UptimeCheckId,
	name: String,
	kind: &'static str,
	target: String,
	method: String,
	expected_status: i32,
	keyword: Option<String>,
	timeout_secs: i32,
	interval_secs: i32,
	tls_warn_days: Option<i32>,
	enabled: bool,
	auto: bool,
	status_visible: bool,
	status_name: Option<String>,
	state: &'static str,
	state_since: Option<String>,
	last_checked_at: Option<String>,
	last_latency_ms: Option<i32>,
	last_error: Option<String>,
	tls_expires_at: Option<String>,
	uptime_percent: Option<f64>,
}

fn stamp(at: Option<OffsetDateTime>) -> Option<String> {
	at.and_then(|t| {
		t.format(&time::format_description::well_known::Rfc3339)
			.ok()
	})
}

fn response(c: &Check, days: &[DayStat]) -> CheckResponse {
	CheckResponse {
		id: c.id,
		name: c.name.clone(),
		kind: c.kind().as_str(),
		target: c.target(),
		method: c.method.clone(),
		expected_status: c.expected_status,
		keyword: c.keyword.clone(),
		timeout_secs: c.timeout_secs,
		interval_secs: c.interval_secs,
		tls_warn_days: c.tls_warn_days,
		enabled: c.enabled,
		auto: c.auto_key.is_some(),
		status_visible: c.status_visible,
		status_name: c.status_name.clone(),
		state: c.state().as_str(),
		state_since: stamp(c.state_since),
		last_checked_at: stamp(c.last_checked_at),
		last_latency_ms: c.last_latency_ms,
		last_error: c.last_error.clone(),
		tls_expires_at: stamp(c.tls_expires_at),
		uptime_percent: uptime_percent(days),
	}
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<CheckResponse>>> {
	let rows =
		service::checks_with_days(&s.db, user.id, team, 30, OffsetDateTime::now_utc()).await?;
	Ok(Json(rows.iter().map(|(c, d)| response(c, d)).collect()))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, UptimeCheckId)>,
) -> AppResult<Json<CheckResponse>> {
	let c = service::check(&s.db, user.id, team, id).await?;
	let days = service::check_days(&s.db, user.id, team, id, 30, OffsetDateTime::now_utc()).await?;
	Ok(Json(response(&c, &days)))
}

async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(input): Json<CheckInput>,
) -> AppResult<(StatusCode, Json<CheckResponse>)> {
	let c = service::create(&s, user.id, team, &input).await?;
	Ok((StatusCode::CREATED, Json(response(&c, &[]))))
}

async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, UptimeCheckId)>,
	Json(input): Json<CheckInput>,
) -> AppResult<Json<CheckResponse>> {
	let c = service::update(&s, user.id, team, id, &input).await?;
	Ok(Json(response(&c, &[])))
}

async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, UptimeCheckId)>,
) -> AppResult<StatusCode> {
	service::remove(&s.db, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}

async fn run(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, UptimeCheckId)>,
) -> AppResult<StatusCode> {
	service::run_now(&s, user.id, team, id).await?;
	Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct RangeQuery {
	range: Option<String>,
}

#[derive(Serialize)]
struct ResultsResponse {
	points: Vec<LatencyPoint>,
}

async fn results(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, UptimeCheckId)>,
	Query(q): Query<RangeQuery>,
) -> AppResult<Json<ResultsResponse>> {
	let range = match q.range.as_deref() {
		None => Range::Day,
		Some(r) => Range::parse(r)
			.ok_or_else(|| ServerError::Validation("range is one of 1h, 24h, 7d".to_owned()))?,
	};
	let points =
		service::series(&s.db, user.id, team, id, range, OffsetDateTime::now_utc()).await?;
	Ok(Json(ResultsResponse { points }))
}

#[derive(Serialize)]
struct IncidentResponse {
	started_at: Option<String>,
	ended_at: Option<String>,
	error: Option<String>,
}

impl From<Incident> for IncidentResponse {
	fn from(i: Incident) -> Self {
		Self {
			started_at: stamp(Some(i.started_at)),
			ended_at: stamp(i.ended_at),
			error: i.error,
		}
	}
}

async fn incidents(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, UptimeCheckId)>,
) -> AppResult<Json<Vec<IncidentResponse>>> {
	let rows = service::incidents(&s.db, user.id, team, id, 50).await?;
	Ok(Json(rows.into_iter().map(Into::into).collect()))
}

#[derive(Serialize, Deserialize)]
struct StatusPageBody {
	enabled: bool,
	#[serde(default)]
	title: String,
	#[serde(default, skip_deserializing)]
	slug: String,
}

async fn get_status_page(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<StatusPageBody>> {
	let page = service::status_page(&s.db, user.id, team).await?;
	Ok(Json(StatusPageBody {
		enabled: page.enabled,
		title: page.title.unwrap_or_default(),
		slug: page.slug,
	}))
}

async fn put_status_page(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<StatusPageBody>,
) -> AppResult<Json<StatusPageBody>> {
	service::set_status_page(&s.db, user.id, team, body.enabled, &body.title).await?;
	get_status_page(State(s), user, Path(team)).await
}

#[derive(Serialize, Deserialize)]
struct SettingsBody {
	auto_checks: bool,
}

async fn get_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<SettingsBody>> {
	Ok(Json(SettingsBody {
		auto_checks: service::auto_checks(&s.db, user.id, team).await?,
	}))
}

async fn put_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<SettingsBody>,
) -> AppResult<Json<SettingsBody>> {
	service::set_auto_checks(&s.db, user.id, team, body.auto_checks).await?;
	Ok(Json(body))
}
