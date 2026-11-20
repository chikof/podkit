//! `/api/v1/teams/{team_id}/security`: results, findings, the SBOM download,
//! ignores and the per-server settings. Every handler goes through `service`.

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::service::{self, NewIgnore, ScanView, Settings};
use super::store::{FindingFilter, StoredFinding, StoredIgnore};
use super::{Badge, BadgeState, Severity, Subject};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, DatabaseId, ModelId, ProjectId, ScanId, ScanIgnoreId, ServerId, StackId, TeamId,
};
use crate::state::AppState;

/// Mounted at `/teams/{team_id}/security`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(overview))
		.route("/scans/{scan_id}", get(scan))
		.route("/scans/{scan_id}/sbom", get(sbom))
		.route("/ignores", get(ignores).post(add_ignore))
		.route("/ignores/{ignore_id}", delete(remove_ignore))
		.route("/servers/{server_id}", get(get_settings).put(put_settings))
		.route("/projects/{project_id}/{kind}/{id}", get(subject))
		.route(
			"/projects/{project_id}/{kind}/{id}/scan",
			axum::routing::post(scan_again),
		)
}

fn time_text(t: OffsetDateTime) -> String {
	t.format(&Rfc3339).unwrap_or_default()
}

fn badge_json(badge: &Badge) -> Value {
	let state = match badge.state {
		BadgeState::None => "none".to_owned(),
		BadgeState::Scanning => "scanning".to_owned(),
		BadgeState::Failed => "failed".to_owned(),
		BadgeState::Clean => "clean".to_owned(),
		BadgeState::Found(s) => s.as_str().to_owned(),
	};
	json!({ "state": state, "counts": badge.counts, "act_now": badge.counts.act_now > 0 })
}

fn scan_json(view: &ScanView) -> Value {
	let s = &view.scan;
	json!({
		"id": s.id,
		"server_id": s.server_id,
		"image_ref": s.image_ref,
		"image_digest": s.image_digest,
		"status": s.status,
		"error": s.error,
		"started_at": s.started_at.map(time_text),
		"finished_at": s.finished_at.map(time_text),
		"rescanned_at": s.rescanned_at.map(time_text),
		"db_built": s.db_built,
		"tools": s.tools,
		"has_sbom": view.has_sbom,
		"counts": view.counts,
		"badge": badge_json(&view.badge),
	})
}

fn finding_json(f: &StoredFinding) -> Value {
	json!({
		"vuln_id": f.vuln_id,
		"package": f.package,
		"version": f.version,
		"fixed_version": f.fixed_version,
		"severity": f.severity,
		"cvss": f.cvss,
		"epss": f.epss,
		"kev": f.kev,
		"act_now": f.act_now,
		"url": f.url,
		"ignored": f.ignored,
	})
}

fn ignore_json(i: &StoredIgnore) -> Value {
	json!({
		"id": i.id,
		"vuln_id": i.vuln_id,
		"package": i.package,
		"reason": i.reason,
		"expires_at": time_text(i.expires_at),
		"active": i.expires_at > OffsetDateTime::now_utc(),
		"created_by": i.created_by_name,
		"created_at": time_text(i.created_at),
	})
}

/// Every resource of the team with its badge, riskiest first.
async fn overview(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Value>> {
	let rows = service::overview(&s.db, user.id, team).await?;
	Ok(Json(json!(
		rows.iter()
			.map(|r| json!({
				"kind": r.kind_label.to_lowercase(),
				"id": match r.subject {
					Subject::Application(i) => i.0,
					Subject::Database(i) => i.0,
					Subject::Model(i) => i.0,
					Subject::Stack(i) => i.0,
				},
				"name": r.name,
				"project_id": r.project_id,
				"project_name": r.project_name,
				"badge": badge_json(&r.badge),
			}))
			.collect::<Vec<_>>()
	)))
}

#[derive(Deserialize, Default)]
struct FindingQuery {
	severity: Option<String>,
	#[serde(default)]
	act_now: bool,
	#[serde(default)]
	fixable: bool,
	#[serde(default)]
	ignored: bool,
}

impl FindingQuery {
	fn filter(&self) -> AppResult<FindingFilter> {
		let severity = match self.severity.as_deref() {
			None | Some("" | "all") => None,
			Some(s) => Some(Severity::parse(s).ok_or_else(|| {
				ServerError::Validation(
					"severity must be critical, high, medium, low or unknown".to_owned(),
				)
			})?),
		};
		Ok(FindingFilter {
			severity,
			act_now: self.act_now,
			fixable: self.fixable,
			show_ignored: self.ignored,
		})
	}
}

async fn scan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ScanId)>,
	Query(query): Query<FindingQuery>,
) -> AppResult<Json<Value>> {
	let (view, findings) = service::findings(&s.db, user.id, team, id, query.filter()?).await?;
	Ok(Json(json!({
		"scan": scan_json(&view),
		"findings": findings.iter().map(finding_json).collect::<Vec<_>>(),
	})))
}

/// The `CycloneDX` JSON of a scan, as a download.
async fn sbom(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ScanId)>,
) -> AppResult<Response> {
	let (name, json) = service::sbom(&s.db, user.id, team, id).await?;
	Ok((
		StatusCode::OK,
		[
			(
				header::CONTENT_TYPE,
				"application/vnd.cyclonedx+json".to_owned(),
			),
			(
				header::CONTENT_DISPOSITION,
				format!("attachment; filename=\"{name}\""),
			),
		],
		json,
	)
		.into_response())
}

async fn ignores(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Value>> {
	let list = service::ignores(&s.db, user.id, team).await?;
	Ok(Json(json!(
		list.iter().map(ignore_json).collect::<Vec<_>>()
	)))
}

#[derive(Deserialize)]
struct IgnoreBody {
	vuln_id: String,
	#[serde(default)]
	package: Option<String>,
	reason: String,
	days: i64,
}

async fn add_ignore(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<IgnoreBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
	let id = service::ignore(
		&s.db,
		user.id,
		team,
		NewIgnore {
			vuln_id: body.vuln_id,
			package: body.package,
			reason: body.reason,
			days: body.days,
		},
	)
	.await?;
	Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn remove_ignore(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ScanIgnoreId)>,
) -> AppResult<StatusCode> {
	service::remove_ignore(&s.db, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<Value>> {
	let settings = service::settings(&s.db, user.id, team, server).await?;
	Ok(Json(json!({
		"enabled": settings.enabled,
		"cpus": settings.cpus,
		"memory_mb": settings.memory_mb,
	})))
}

#[derive(Deserialize)]
struct SettingsBody {
	enabled: bool,
	cpus: f64,
	memory_mb: i32,
}

async fn put_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
	Json(body): Json<SettingsBody>,
) -> AppResult<StatusCode> {
	service::update_settings(
		&s.db,
		user.id,
		team,
		server,
		Settings {
			enabled: body.enabled,
			cpus: body.cpus,
			memory_mb: body.memory_mb,
		},
	)
	.await?;
	Ok(StatusCode::NO_CONTENT)
}

fn subject_of(kind: &str, id: i64) -> AppResult<Subject> {
	match kind {
		"applications" => Ok(Subject::Application(ApplicationId(id))),
		"databases" => Ok(Subject::Database(DatabaseId(id))),
		"models" => Ok(Subject::Model(ModelId(id))),
		"stacks" => Ok(Subject::Stack(StackId(id))),
		_ => Err(ServerError::NotFound),
	}
}

async fn subject(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, kind, id)): Path<(TeamId, ProjectId, String, i64)>,
) -> AppResult<Json<Value>> {
	let found =
		service::subject_scans(&s.db, user.id, team, project, subject_of(&kind, id)?).await?;
	Ok(Json(json!({
		"scanning_on": found.scanning_on,
		"badge": badge_json(&found.badge),
		"entries": found.entries.iter().map(|e| json!({
			"service": e.label,
			"scan": e.view.as_ref().map(scan_json),
		})).collect::<Vec<_>>(),
	})))
}

async fn scan_again(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, kind, id)): Path<(TeamId, ProjectId, String, i64)>,
) -> AppResult<(StatusCode, Json<Value>)> {
	let queued = service::scan_again(&s, user.id, team, project, subject_of(&kind, id)?).await?;
	Ok((StatusCode::ACCEPTED, Json(json!({ "queued": queued }))))
}
