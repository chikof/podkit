use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::service::{self, Connection, CreateInput, SettingsInput};
use super::{Database, DatabaseStatus, EngineKind};
use crate::applications::routes::log_events;
use crate::auth::AuthUser;
use crate::environments::EnvironmentFilter;
use crate::error::AppResult;
use crate::ids::{BackupTargetId, DatabaseId, EnvironmentId, ProjectId, ServerId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{database_id}", get(get_one).delete(remove))
		.route("/{database_id}/settings", put(update_settings))
		.route("/{database_id}/start", post(start))
		.route("/{database_id}/stop", post(stop))
		.route("/{database_id}/restart", post(restart))
		.route("/{database_id}/logs", get(logs))
}

#[derive(Deserialize)]
struct CreateDatabaseRequest {
	name: String,
	engine: EngineKind,
	version: Option<String>,
	server_id: ServerId,
	public_port: Option<i32>,
	memory_limit_mb: Option<i32>,
	cpu_limit: Option<f64>,
	#[serde(default)]
	environment_id: Option<EnvironmentId>,
}

/// Replaces every editable setting; omitted fields become null (`backup_keep`
/// falls back to its default).
#[derive(Deserialize)]
struct SettingsRequest {
	public_port: Option<i32>,
	memory_limit_mb: Option<i32>,
	cpu_limit: Option<f64>,
	backup_target_id: Option<BackupTargetId>,
	backup_schedule: Option<String>,
	backup_keep: Option<i32>,
}

#[derive(Deserialize)]
struct RemoveParams {
	#[serde(default)]
	purge_data: bool,
}

#[derive(Deserialize)]
struct LogsParams {
	#[serde(default = "default_tail")]
	tail: u32,
	#[serde(default = "default_follow")]
	follow: bool,
}

fn default_tail() -> u32 {
	200
}

fn default_follow() -> bool {
	true
}

/// Never carries the stored credentials; `connection` is added by `get_one`
/// for callers allowed to see them.
#[derive(Serialize)]
struct DatabaseResponse {
	id: DatabaseId,
	project_id: ProjectId,
	environment_id: EnvironmentId,
	server_id: ServerId,
	name: String,
	engine: EngineKind,
	version: String,
	public_port: Option<i32>,
	memory_limit_mb: Option<i32>,
	cpu_limit: Option<f64>,
	status: DatabaseStatus,
	status_message: Option<String>,
	container_id: Option<String>,
	backup_target_id: Option<BackupTargetId>,
	backup_schedule: Option<String>,
	backup_keep: i32,
	/// Unix seconds.
	backup_next_run_at: Option<i64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	connection: Option<Connection>,
}

impl From<&Database> for DatabaseResponse {
	fn from(d: &Database) -> Self {
		Self {
			id: d.id,
			project_id: d.project_id,
			environment_id: d.environment_id,
			server_id: d.server_id,
			name: d.name.clone(),
			engine: d.engine,
			version: d.version.clone(),
			public_port: d.public_port,
			memory_limit_mb: d.memory_limit_mb,
			cpu_limit: d.cpu_limit.map(f64::from),
			status: d.status,
			status_message: d.status_message.clone(),
			container_id: d.container_id.clone(),
			backup_target_id: d.backup_target_id,
			backup_schedule: d.backup_schedule.clone(),
			backup_keep: d.backup_keep,
			backup_next_run_at: d.backup_next_run_at.map(OffsetDateTime::unix_timestamp),
			connection: None,
		}
	}
}

fn accepted(database: &Database) -> (StatusCode, Json<DatabaseResponse>) {
	(StatusCode::ACCEPTED, Json(DatabaseResponse::from(database)))
}

/// Creates a database row and queues its first provision; poll `GET` until
/// `status` leaves `pending`, or follow the job log through `.../logs`.
/// Caller must hold `create database` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<CreateDatabaseRequest>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	let input = CreateInput {
		name: body.name,
		engine: body.engine,
		version: body.version,
		server_id: body.server_id,
		public_port: body.public_port,
		memory_limit_mb: body.memory_limit_mb,
		cpu_limit: body.cpu_limit,
		environment_id: body.environment_id,
	};
	let database = service::create(&s, user.id, team, project, input).await?;
	Ok(accepted(&database))
}

/// Lists a project's databases without their credentials. Caller must hold
/// `read database`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(filter): Query<EnvironmentFilter>,
) -> AppResult<Json<Vec<DatabaseResponse>>> {
	let rows = service::list(&s.db, user.id, team, project, filter.environment_id).await?;
	Ok(Json(rows.iter().map(DatabaseResponse::from).collect()))
}

/// Fetches one database. The connection details, password included, are only
/// returned to callers who also hold `update database`.
async fn get_one(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<Json<DatabaseResponse>> {
	let (database, connection) =
		service::get_with_connection(&s, user.id, team, project, id).await?;
	let mut response = DatabaseResponse::from(&database);
	// Passwords come out only for a session or a token with `read:sensitive`.
	response.connection = connection.filter(|_| user.can_reveal());
	Ok(Json(response))
}

/// Replaces the database's settings. A changed public port or resource limit
/// reprovisions the container. Caller must hold `update database`.
async fn update_settings(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
	Json(body): Json<SettingsRequest>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	let input = SettingsInput {
		public_port: body.public_port,
		memory_limit_mb: body.memory_limit_mb,
		cpu_limit: body.cpu_limit,
		backup_target_id: body.backup_target_id,
		backup_schedule: body.backup_schedule,
		backup_keep: body.backup_keep,
	};
	let (database, reprovisioned) =
		service::update_settings(&s, user.id, team, project, id, input).await?;
	let status = if reprovisioned {
		StatusCode::ACCEPTED
	} else {
		StatusCode::OK
	};
	Ok((status, Json(DatabaseResponse::from(&database))))
}

/// Starts the stopped container. Caller must hold `update database`.
async fn start(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	let database = service::start(&s, user.id, team, project, id).await?;
	Ok(accepted(&database))
}

/// Stops the container; the agent will not restart it. Caller must hold
/// `update database`.
async fn stop(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	let database = service::stop(&s, user.id, team, project, id).await?;
	Ok(accepted(&database))
}

/// Recreates the container from the stored settings, keeping its data.
/// Caller must hold `update database`.
async fn restart(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	let database = service::restart(&s, user.id, team, project, id).await?;
	Ok(accepted(&database))
}

/// Marks the database `deleting` and queues the removal of its containers;
/// `?purge_data=true` also deletes the data volume. The row disappears when
/// the job finishes. Caller must hold `delete database`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(params): Query<RemoveParams>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	let database = service::remove(&s, user.id, team, project, id, params.purge_data).await?;
	Ok(accepted(&database))
}

/// Streams the container's stdout/stderr as server-sent events
/// (`event: log`, data `{"stream","line"}`, then `event: end`), exactly like
/// application runtime logs. `?tail=` is capped at 5000 lines. Caller must
/// hold `read database`.
async fn logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(params): Query<LogsParams>,
) -> AppResult<impl IntoResponse> {
	let events =
		service::container_logs(&s, user.id, team, project, id, params.tail, params.follow).await?;
	Ok(Sse::new(log_events(events, s.shutdown.clone())).keep_alive(KeepAlive::default()))
}
