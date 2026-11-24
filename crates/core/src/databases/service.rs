//! Database use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or the agent.

use axum::http::Method;
use futures_util::stream::BoxStream;
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{
	Credentials, Database, DatabaseStatus, EngineKind, NAME_CONSTRAINT, NewDatabase,
	PORT_CONSTRAINT, SettingsUpdate, engines,
};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{
	BackupTargetId, DatabaseId, EnvironmentId, JobId, ProjectId, ServerId, TeamId, UserId,
};
use crate::jobs::{self, JobKind, JobStatus};
use crate::servers::{self, Server};
use crate::state::AppState;
use crate::{backups, databases, db, environments, scope};

const MAX_TAIL: u32 = 5000;
const DEFAULT_BACKUP_KEEP: i32 = 7;

pub struct CreateInput {
	pub name: String,
	pub engine: EngineKind,
	pub version: Option<String>,
	pub server_id: ServerId,
	pub public_port: Option<i32>,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	/// The project's first environment when `None`.
	pub environment_id: Option<EnvironmentId>,
}

/// Replaces every editable setting; omitted fields become null (`backup_keep`
/// falls back to its default).
pub struct SettingsInput {
	pub public_port: Option<i32>,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	pub backup_target_id: Option<BackupTargetId>,
	pub backup_schedule: Option<String>,
	pub backup_keep: Option<i32>,
}

/// How to reach a database, password included. Only for callers who may
/// update it.
#[derive(Serialize)]
pub struct Connection {
	pub internal_url: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub public_url: Option<String>,
	pub username: String,
	pub password: String,
	pub database: String,
}

/// The non-secret half of [`Connection`].
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
	pub internal_host: String,
	pub port: u16,
	pub username: String,
	pub database: String,
	/// Host and published port, when a public port is set.
	pub public: Option<(String, u16)>,
}

/// A database with what a page about it shows. Carries no credentials.
#[derive(Debug, Clone)]
pub struct DatabaseDetail {
	pub database: Database,
	pub server_name: String,
	/// The newest lifecycle job, if any ever ran.
	pub job: Option<(JobId, JobStatus)>,
}

/// What the create form offers for one engine.
#[derive(Debug, Clone)]
pub struct EngineOption {
	pub kind: EngineKind,
	pub label: &'static str,
	pub default_version: &'static str,
	pub versions: &'static [&'static str],
}

#[must_use]
pub fn engine_options() -> Vec<EngineOption> {
	[
		(EngineKind::Postgres, "PostgreSQL"),
		(EngineKind::Mysql, "MySQL"),
		(EngineKind::Mongodb, "MongoDB"),
		(EngineKind::Redis, "Redis"),
	]
	.into_iter()
	.map(|(kind, label)| {
		let spec = engines::spec(kind.into());
		EngineOption {
			kind,
			label,
			default_version: spec.default_version(),
			versions: spec.versions,
		}
	})
	.collect()
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

/// How many upcoming runs the schedule preview lists.
pub const PREVIEW_RUNS: usize = 3;

/// The first `count` occurrences of a backup schedule, for the preview under the field.
///
/// # Errors
/// The parser's message when `schedule` is not a valid 5-field cron expression.
pub fn upcoming_runs(schedule: &str, count: usize) -> Result<Vec<OffsetDateTime>, String> {
	backups::parse_schedule(schedule)?;
	let mut runs = Vec::with_capacity(count);
	let mut after = OffsetDateTime::now_utc();
	for _ in 0..count {
		let next = backups::next_run(schedule, after)?;
		runs.push(next);
		after = next;
	}
	Ok(runs)
}

fn credentials_of(s: &AppState, database: &Database) -> AppResult<Credentials> {
	databases::decrypt_credentials(&s.secrets, database).map_err(|e| {
		tracing::error!(database = database.id.0, "{e}");
		ServerError::Internal
	})
}

fn internal_host(database: &Database) -> String {
	format!("podkit-db-{}", database.id)
}

/// Queues `kind` for `database`.
async fn queue(s: &AppState, kind: JobKind, database: &Database) -> AppResult<()> {
	databases::enqueue(s, kind, database).await?;
	Ok(())
}

/// Queues `kind`; if that fails the row is marked failed, so it doesn't sit
/// in a state no job will ever move it out of.
async fn queue_or_fail(
	s: &AppState,
	kind: JobKind,
	database: &Database,
	message: &str,
) -> AppResult<()> {
	if let Err(e) = databases::enqueue(s, kind, database).await {
		if let Err(mark) =
			databases::set_status(&s.db, database.id, DatabaseStatus::Failed, Some(message)).await
		{
			tracing::warn!(
				database = database.id.0,
				"failed to mark unqueued database failed: {mark}"
			);
		}
		return Err(e);
	}
	Ok(())
}

/// The team's servers that take new resources (not draining), for the create form's
/// server select; needs `create database`.
pub async fn servers_for_create(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Server>> {
	authorize(db, user, team, Action::Create, Resource::Database).await?;
	crate::moves::open_servers(db, servers::list_by_team(db, team).await?).await
}

/// The team's backup targets as (id, name), for the settings form; needs
/// `update database`. Nothing else about a target leaves here.
pub async fn backup_targets(
	db: &PgPool,
	user: UserId,
	team: TeamId,
) -> AppResult<Vec<(BackupTargetId, String)>> {
	authorize(db, user, team, Action::Update, Resource::Database).await?;
	Ok(backups::targets::list_for_team(db, team)
		.await?
		.into_iter()
		.map(|t| (t.id, t.name))
		.collect())
}

/// Creates a database row and queues its first provision. Caller must hold
/// `create database` on the team.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	body: CreateInput,
) -> AppResult<Database> {
	authorize(&s.db, user, team, Action::Create, Resource::Database).await?;
	scope::project(&s.db, team, project).await?;
	let environment =
		environments::service::resolve(&s.db, team, project, body.environment_id).await?;
	let server = scope::server(&s.db, team, body.server_id).await?;
	crate::moves::ensure_open(&s.db, &server).await?;

	if body.name.trim().is_empty() || body.name.len() > 60 {
		return Err(ServerError::Validation(
			"name must be 1 to 60 characters".to_string(),
		));
	}
	let version = engines::spec(body.engine.into()).resolve_version(body.version.as_deref())?;
	validate_public_port(s, body.public_port)?;
	crate::quotas::service::check_create(
		s,
		team,
		"database",
		&crate::quotas::service::reserved(
			crate::quotas::QuotaKind::Databases,
			body.memory_limit_mb,
			body.cpu_limit,
			false,
		),
	)
	.await?;

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
			environment_id: environment,
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

	queue_or_fail(
		s,
		JobKind::DatabaseProvision,
		&database,
		"failed to queue provision job",
	)
	.await?;
	audit::log(
		&s.db,
		user,
		team,
		"database.create",
		Target::new("database", database.id)
			.named(&database.name)
			.in_project(project),
		Diff::new()
			.set("name", database.name.clone())
			.set("engine", format!("{:?}", database.engine).to_lowercase())
			.set("version", database.version.clone())
			.set("server_id", server.id.0)
			.set("public_port", database.public_port)
			.secret("password"),
	)
	.await;
	Ok(database)
}

/// Lists the databases of one environment of a project, the first when
/// `environment` is `None`; needs `read database`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: Option<EnvironmentId>,
) -> AppResult<Vec<Database>> {
	authorize(db, user, team, Action::Read, Resource::Database).await?;
	scope::project(db, team, project).await?;
	let environment = environments::service::resolve(db, team, project, environment).await?;
	Ok(databases::list_by_environment(db, environment).await?)
}

/// One database without its credentials; needs `read database`.
pub async fn find(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Database> {
	authorize(db, user, team, Action::Read, Resource::Database).await?;
	scope::database(db, team, project, id).await
}

/// One database with its server and newest lifecycle job; needs
/// `read database`.
pub async fn detail(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<DatabaseDetail> {
	let database = find(db, user, team, project, id).await?;
	let server = scope::server(db, team, database.server_id).await?;
	let job = jobs::latest_for_database(db, database.id.0).await?;
	Ok(DatabaseDetail {
		database,
		server_name: server.name,
		job: job.map(|j| (j.id, j.status)),
	})
}

/// The newest lifecycle job of a database, whose log the page streams;
/// needs `read database`.
pub async fn latest_job(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Option<JobId>> {
	let database = find(db, user, team, project, id).await?;
	Ok(jobs::latest_for_database(db, database.id.0)
		.await?
		.map(|j| j.id))
}

fn connection_of(
	database: &Database,
	server: &Server,
	credentials: Credentials,
) -> (Connection, ConnectionInfo) {
	let engine = engines::spec(database.engine());
	let host = internal_host(database);
	let public_port = database
		.public_port
		.and_then(|port| u16::try_from(port).ok());
	let connection = Connection {
		internal_url: engine.url(&host, engine.port, &credentials),
		public_url: public_port.map(|port| engine.url(&server.hostname, port, &credentials)),
		username: credentials.username.clone(),
		password: credentials.password,
		database: credentials.database.clone(),
	};
	let info = ConnectionInfo {
		internal_host: host,
		port: engine.port,
		username: credentials.username,
		database: credentials.database,
		public: public_port.map(|port| (server.hostname.clone(), port)),
	};
	(connection, info)
}

/// One database; the connection details, password included, only for
/// callers who also hold `update database`.
pub async fn get_with_connection(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<(Database, Option<Connection>)> {
	let database = find(&s.db, user, team, project, id).await?;
	match authorize(&s.db, user, team, Action::Update, Resource::Database).await {
		Ok(()) => {
			let server = scope::server(&s.db, team, database.server_id).await?;
			let credentials = credentials_of(s, &database)?;
			let (connection, _) = connection_of(&database, &server, credentials);
			Ok((database, Some(connection)))
		}
		Err(ServerError::Forbidden) => Ok((database, None)),
		Err(e) => Err(e),
	}
}

/// What the connection panel shows without a click: host, port, user and
/// database, never the password. `None` for callers without
/// `update database`, who see no connection details at all.
pub async fn connection_info(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Option<ConnectionInfo>> {
	match get_with_connection(s, user, team, project, id).await? {
		(_, None) => Ok(None),
		(database, Some(_)) => {
			let server = scope::server(&s.db, team, database.server_id).await?;
			let credentials = credentials_of(s, &database)?;
			Ok(Some(connection_of(&database, &server, credentials).1))
		}
	}
}

/// The password and URLs of one database. Needs `read database` and
/// `update database`, exactly like the JSON API's `connection`.
pub async fn reveal(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Connection> {
	match get_with_connection(s, user, team, project, id).await? {
		(database, Some(connection)) => {
			// The password is not written down, only that it was shown.
			audit::log(
				&s.db,
				user,
				team,
				"database.reveal",
				Target::new("database", database.id)
					.named(&database.name)
					.in_project(project),
				Diff::new().secret("password"),
			)
			.await;
			Ok(connection)
		}
		(_, None) => Err(ServerError::Forbidden),
	}
}

/// Checks `schedule` for the settings form's inline message and returns its next
/// three runs; needs `update database` on a database of the team.
pub async fn preview_schedule(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
	schedule: &str,
) -> AppResult<Result<Vec<OffsetDateTime>, String>> {
	authorize(db, user, team, Action::Update, Resource::Database).await?;
	scope::database(db, team, project, id).await?;
	Ok(upcoming_runs(schedule, PREVIEW_RUNS))
}

/// Replaces the database's settings. A changed public port or resource limit
/// reprovisions the container; the flag says whether it was queued. Caller
/// must hold `update database`.
pub async fn update_settings(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
	body: SettingsInput,
) -> AppResult<(Database, bool)> {
	authorize(&s.db, user, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;

	validate_public_port(s, body.public_port)?;
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
		queue(s, JobKind::DatabaseProvision, &updated).await?;
	}
	audit::log(
		&s.db,
		user,
		team,
		"database.update",
		Target::new("database", updated.id)
			.named(&updated.name)
			.in_project(project),
		Diff::new()
			.changed("public_port", database.public_port, updated.public_port)
			.changed(
				"memory_limit_mb",
				database.memory_limit_mb,
				updated.memory_limit_mb,
			)
			.changed("cpu_limit", database.cpu_limit, updated.cpu_limit)
			.changed(
				"backup_target_id",
				database.backup_target_id.map(|t| t.0),
				updated.backup_target_id.map(|t| t.0),
			)
			.changed(
				"backup_schedule",
				database.backup_schedule.clone(),
				updated.backup_schedule.clone(),
			)
			.changed("backup_keep", database.backup_keep, updated.backup_keep)
			.set("reprovisioned", reprovision),
	)
	.await;
	Ok((updated, reprovision))
}

async fn lifecycle(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
	kind: JobKind,
	action: &str,
) -> AppResult<Database> {
	authorize(&s.db, user, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	if matches!(kind, JobKind::DatabaseStart) {
		crate::quotas::service::check_deploy(s, team, "database").await?;
	}
	queue(s, kind, &database).await?;
	audit::log(
		&s.db,
		user,
		team,
		action,
		Target::new("database", database.id)
			.named(&database.name)
			.in_project(project),
		Diff::new(),
	)
	.await;
	Ok(database)
}

/// Starts the stopped container. Caller must hold `update database`.
pub async fn start(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Database> {
	lifecycle(
		s,
		user,
		team,
		project,
		id,
		JobKind::DatabaseStart,
		"database.start",
	)
	.await
}

/// Stops the container; the agent will not restart it. Caller must hold
/// `update database`.
pub async fn stop(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Database> {
	lifecycle(
		s,
		user,
		team,
		project,
		id,
		JobKind::DatabaseStop,
		"database.stop",
	)
	.await
}

/// Recreates the container from the stored settings, keeping its data.
/// Caller must hold `update database`.
pub async fn restart(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Database> {
	lifecycle(
		s,
		user,
		team,
		project,
		id,
		JobKind::DatabaseProvision,
		"database.restart",
	)
	.await
}

/// Marks the database `deleting` and queues the removal of its containers;
/// `purge_data` also deletes the data volume. The row disappears when the job
/// finishes. Caller must hold `delete database`.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
	purge_data: bool,
) -> AppResult<Database> {
	authorize(&s.db, user, team, Action::Delete, Resource::Database).await?;
	let mut database = scope::database(&s.db, team, project, id).await?;

	databases::set_status(&s.db, database.id, DatabaseStatus::Deleting, None).await?;
	database.status = DatabaseStatus::Deleting;
	database.status_message = None;

	let kind = if purge_data {
		JobKind::DatabasePurge
	} else {
		JobKind::DatabaseRemove
	};
	queue_or_fail(s, kind, &database, "failed to queue remove job").await?;
	audit::log(
		&s.db,
		user,
		team,
		"database.delete",
		Target::new("database", database.id)
			.named(&database.name)
			.in_project(project),
		Diff::new()
			.set("name", database.name.clone())
			.set("purge_data", purge_data),
	)
	.await;
	Ok(database)
}

/// The container's log from its agent. `tail` is capped at 5000 lines.
/// Needs `read database`; the database must be running.
pub async fn container_logs(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
	tail: u32,
	follow: bool,
) -> AppResult<BoxStream<'static, Result<proto::Event, ServerError>>> {
	let database = find(&s.db, user, team, project, id).await?;
	let Some(container_id) = database.container_id.as_deref() else {
		return Err(ServerError::Validation(
			"database is not running".to_string(),
		));
	};

	let server = scope::server(&s.db, team, database.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let path = format!(
		"/v1/containers/{container_id}/logs?tail={}&follow={follow}",
		tail.min(MAX_TAIL)
	);
	client.stream(Method::GET, &path, None::<&()>).await
}
