//! Restore drills of one database, under
//! `/api/v1/teams/{team}/projects/{project}/databases/{database}/drills`.
//! The use cases are in [`super::service`].

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::schedule::Settings;
use super::{Drill, DrillStatus, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{BackupId, DatabaseId, ProjectId, RestoreDrillId, TeamId, UserId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list).post(run_now))
		.route("/schedule", get(schedule).put(set_schedule))
}

#[derive(Serialize)]
pub struct DrillResponse {
	pub id: RestoreDrillId,
	pub database_id: DatabaseId,
	pub backup_id: Option<BackupId>,
	pub backup_taken_at: Option<String>,
	pub status: DrillStatus,
	pub tables_found: Option<i64>,
	pub source_tables: Option<i64>,
	pub error: Option<String>,
	pub triggered_by: Option<UserId>,
	pub created_at: String,
	pub started_at: Option<String>,
	pub finished_at: Option<String>,
	pub duration_ms: Option<i64>,
}

impl From<&Drill> for DrillResponse {
	fn from(d: &Drill) -> Self {
		Self {
			id: d.id,
			database_id: d.database_id,
			backup_id: d.backup_id,
			backup_taken_at: d.backup_taken_at.map(|t| t.to_string()),
			status: d.status,
			tables_found: d.tables_found,
			source_tables: d.source_tables,
			error: d.error.clone(),
			triggered_by: d.triggered_by,
			created_at: d.created_at.to_string(),
			started_at: d.started_at.map(|t| t.to_string()),
			finished_at: d.finished_at.map(|t| t.to_string()),
			duration_ms: d.duration_ms,
		}
	}
}

#[derive(Serialize)]
struct ScheduleResponse {
	schedule: Option<String>,
	next_run_at: Option<String>,
}

impl From<&Settings> for ScheduleResponse {
	fn from(s: &Settings) -> Self {
		Self {
			schedule: s.schedule.clone(),
			next_run_at: s.next_run_at.map(|t| t.to_string()),
		}
	}
}

#[derive(Deserialize)]
struct ListParams {
	limit: Option<i64>,
	before: Option<RestoreDrillId>,
}

#[derive(Deserialize)]
struct ScheduleBody {
	/// A five-field UTC cron; `null` or blank turns scheduled drills off.
	schedule: Option<String>,
}

/// Lists a database's drills, newest first, paged with `?limit=` (1..=200, default 20)
/// and `?before=<drill id>`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<DrillResponse>>> {
	let rows = service::list(
		&s.db,
		user.id,
		team,
		project,
		database,
		params.before,
		params.limit,
	)
	.await?;
	Ok(Json(rows.iter().map(DrillResponse::from).collect()))
}

/// Queues a drill with the newest succeeded backup and returns immediately. Poll the list
/// for the outcome. Caller must hold `create backup` on the team.
async fn run_now(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DrillResponse>)> {
	let drill = service::run_now(&s, user.id, team, project, database).await?;
	Ok((StatusCode::ACCEPTED, Json(DrillResponse::from(&drill))))
}

/// The drill schedule and when it next fires.
async fn schedule(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<Json<ScheduleResponse>> {
	let settings = service::settings(&s.db, user.id, team, project, database).await?;
	Ok(Json(ScheduleResponse::from(&settings)))
}

/// Sets or clears the drill schedule. Caller must hold `update database` on the team.
async fn set_schedule(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	Json(body): Json<ScheduleBody>,
) -> AppResult<Json<ScheduleResponse>> {
	let settings = service::set_schedule(
		&s,
		user.id,
		team,
		project,
		database,
		body.schedule.as_deref(),
	)
	.await?;
	Ok(Json(ScheduleResponse::from(&settings)))
}
