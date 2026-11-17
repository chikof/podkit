use axum::extract::{Path, Query, State};
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{
	Credentials, Database, DatabaseStatus, EngineKind, NAME_CONSTRAINT, NewDatabase,
	PORT_CONSTRAINT, SettingsUpdate, engines,
};
use crate::applications::routes::log_events;
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{BackupTargetId, DatabaseId, ProjectId, ServerId, TeamId};
use crate::jobs::JobKind;
use crate::state::AppState;
use crate::{backups, databases, db, scope};

const MAX_TAIL: u32 = 5000;
const DEFAULT_BACKUP_KEEP: i32 = 7;

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

#[derive(Serialize)]
struct Connection {
	internal_url: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	public_url: Option<String>,
	username: String,
	password: String,
	database: String,
}

/// Never carries the stored credentials; `connection` is added by `get_one`
/// for callers allowed to see them.
#[derive(Serialize)]
struct DatabaseResponse {
	id: DatabaseId,
	project_id: ProjectId,
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

#[allow(clippy::cast_possible_truncation)]
fn cpu_to_f32(cpu: Option<f64>) -> Option<f32> {
	cpu.map(|c| c as f32)
}

fn unique_conflict(e: sqlx::Error) -> ServerError {
	if db::is_unique_violation(&e, PORT_CONSTRAINT) {
		ServerError::Conflict("public port is already used on this server".to_string())
	} else if db::is_unique_violation(&e, NAME_CONSTRAINT) {
		ServerError::Conflict(
			"a database with this name already exists in this project".to_string(),
		)
	} else {
		e.into()
	}
}

/// The ingress ports belong to Traefik, so a database cannot publish them.
fn validate_public_port(state: &AppState, port: Option<i32>) -> AppResult<()> {
	let Some(port) = port else {
		return Ok(());
	};
	let config = &state.config;
	if !(1024..=65535).contains(&port)
		|| port == i32::from(config.ingress_port)
		|| port == i32::from(config.https_port)
	{
		return Err(ServerError::Validation(format!(
			"public_port must be between 1024 and 65535 and not {} or {}, which the ingress uses",
			config.ingress_port, config.https_port
		)));
	}
	Ok(())
}

/// Queues `kind` for `database`; the caller sees the row as it is now.
async fn queue(
	s: &AppState,
	kind: JobKind,
	database: &Database,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	databases::enqueue(s, kind, database).await?;
	Ok((StatusCode::ACCEPTED, Json(DatabaseResponse::from(database))))
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
	authorize(&s.db, user.id, team, Action::Create, Resource::Database).await?;
	scope::project(&s.db, team, project).await?;
	let server = scope::server(&s.db, team, body.server_id).await?;

	if body.name.trim().is_empty() || body.name.len() > 60 {
		return Err(ServerError::Validation(
			"name must be 1 to 60 characters".to_string(),
		));
	}
	let version = engines::spec(body.engine.into()).resolve_version(body.version.as_deref())?;
	validate_public_port(&s, body.public_port)?;

	let credentials = serde_json::to_string(&Credentials::generate()).map_err(|e| {
		tracing::error!("failed to encode database credentials: {e}");
		ServerError::Internal
	})?;
	let credentials = s
		.secrets
		.encrypt(&credentials)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt credentials: {e}")))?;

	let database = databases::insert(
		&s.db,
		NewDatabase {
			id: DatabaseId::generate(),
			project_id: project,
			server_id: server.id,
			name: body.name,
			engine: body.engine,
			version: version.to_owned(),
			credentials,
			public_port: body.public_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: cpu_to_f32(body.cpu_limit),
		},
	)
	.await
	.map_err(unique_conflict)?;

	if let Err(e) = databases::enqueue(&s, JobKind::DatabaseProvision, &database).await {
		if let Err(mark) = databases::set_status(
			&s.db,
			database.id,
			DatabaseStatus::Failed,
			Some("failed to queue provision job"),
		)
		.await
		{
			tracing::warn!(
				database = database.id.0,
				"failed to mark unqueued database failed: {mark}"
			);
		}
		return Err(e);
	}
	Ok((
		StatusCode::ACCEPTED,
		Json(DatabaseResponse::from(&database)),
	))
}

/// Lists a project's databases without their credentials. Caller must hold
/// `read database`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<Vec<DatabaseResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Database).await?;
	scope::project(&s.db, team, project).await?;

	let rows = databases::list_by_project(&s.db, project).await?;
	Ok(Json(rows.iter().map(DatabaseResponse::from).collect()))
}

/// Fetches one database. The connection details, password included, are only
/// returned to callers who also hold `update database`.
async fn get_one(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<Json<DatabaseResponse>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	let mut response = DatabaseResponse::from(&database);

	match authorize(&s.db, user.id, team, Action::Update, Resource::Database).await {
		Ok(()) => {
			let server = scope::server(&s.db, team, database.server_id).await?;
			let credentials =
				databases::decrypt_credentials(&s.secrets, &database).map_err(|e| {
					tracing::error!(database = database.id.0, "{e}");
					ServerError::Internal
				})?;
			let engine = engines::spec(database.engine());
			response.connection = Some(Connection {
				internal_url: engine.url(
					&format!("podkit-db-{}", database.id),
					engine.port,
					&credentials,
				),
				public_url: database
					.public_port
					.and_then(|port| u16::try_from(port).ok())
					.map(|port| engine.url(&server.hostname, port, &credentials)),
				username: credentials.username,
				password: credentials.password,
				database: credentials.database,
			});
		}
		Err(ServerError::Forbidden) => {}
		Err(e) => return Err(e),
	}
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
	authorize(&s.db, user.id, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;

	validate_public_port(&s, body.public_port)?;
	let backup_keep = body.backup_keep.unwrap_or(DEFAULT_BACKUP_KEEP);
	if !(1..=365).contains(&backup_keep) {
		return Err(ServerError::Validation(
			"backup_keep must be between 1 and 365".to_string(),
		));
	}
	if let Some(target) = body.backup_target_id {
		scope::backup_target(&s.db, team, target).await?;
	}
	let backup_next_run_at = match body.backup_schedule.as_deref() {
		Some(schedule) => {
			backups::parse_schedule(schedule)
				.map_err(|e| ServerError::Validation(format!("invalid backup_schedule: {e}")))?;
			Some(
				backups::next_run(schedule, OffsetDateTime::now_utc()).map_err(|e| {
					ServerError::Validation(format!("invalid backup_schedule: {e}"))
				})?,
			)
		}
		None => None,
	};

	let cpu_limit = cpu_to_f32(body.cpu_limit);
	let reprovision = database.public_port != body.public_port
		|| database.memory_limit_mb != body.memory_limit_mb
		|| database.cpu_limit != cpu_limit;

	let updated = databases::update_settings(
		&s.db,
		database.id,
		SettingsUpdate {
			public_port: body.public_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit,
			backup_target_id: body.backup_target_id,
			backup_schedule: body.backup_schedule,
			backup_keep,
			backup_next_run_at,
		},
	)
	.await
	.map_err(unique_conflict)?;

	if reprovision {
		return queue(&s, JobKind::DatabaseProvision, &updated).await;
	}
	Ok((StatusCode::OK, Json(DatabaseResponse::from(&updated))))
}

/// Starts the stopped container. Caller must hold `update database`.
async fn start(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	authorize(&s.db, user.id, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	queue(&s, JobKind::DatabaseStart, &database).await
}

/// Stops the container; the agent will not restart it. Caller must hold
/// `update database`.
async fn stop(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	authorize(&s.db, user.id, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	queue(&s, JobKind::DatabaseStop, &database).await
}

/// Recreates the container from the stored settings, keeping its data.
/// Caller must hold `update database`.
async fn restart(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<DatabaseResponse>)> {
	authorize(&s.db, user.id, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	queue(&s, JobKind::DatabaseProvision, &database).await
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
	authorize(&s.db, user.id, team, Action::Delete, Resource::Database).await?;
	let mut database = scope::database(&s.db, team, project, id).await?;

	databases::set_status(&s.db, database.id, DatabaseStatus::Deleting, None).await?;
	database.status = DatabaseStatus::Deleting;
	database.status_message = None;

	let kind = if params.purge_data {
		JobKind::DatabasePurge
	} else {
		JobKind::DatabaseRemove
	};
	if let Err(e) = databases::enqueue(&s, kind, &database).await {
		if let Err(mark) = databases::set_status(
			&s.db,
			database.id,
			DatabaseStatus::Failed,
			Some("failed to queue remove job"),
		)
		.await
		{
			tracing::warn!(
				database = database.id.0,
				"failed to mark unqueued database failed: {mark}"
			);
		}
		return Err(e);
	}
	Ok((
		StatusCode::ACCEPTED,
		Json(DatabaseResponse::from(&database)),
	))
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
	authorize(&s.db, user.id, team, Action::Read, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	let Some(container_id) = database.container_id.as_deref() else {
		return Err(ServerError::Validation(
			"database is not running".to_string(),
		));
	};

	let server = scope::server(&s.db, team, database.server_id).await?;
	let client = s.agents.client(&s, &server).await?;
	let path = format!(
		"/v1/containers/{container_id}/logs?tail={}&follow={}",
		params.tail.min(MAX_TAIL),
		params.follow
	);
	let events = client.stream(Method::GET, &path, None::<&()>).await?;

	Ok(Sse::new(log_events(events)).keep_alive(KeepAlive::default()))
}
