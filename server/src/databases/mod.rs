//! Managed databases: one container per row, run on one of the team's
//! servers through the job queue. Rows live in the `databases` table.

pub mod engines;
mod routes;
pub mod run;
pub mod service;

use crypto::age::SecretBox;
use proto::Engine;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::error::ServerError;
use crate::ids::{BackupTargetId, DatabaseId, ProjectId, ServerId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;

pub use routes::router;

/// Unique constraint on `databases (server_id, public_port)`.
pub const PORT_CONSTRAINT: &str = "databases_server_id_public_port_key";
/// Unique constraint on `databases (project_id, name)`.
pub const NAME_CONSTRAINT: &str = "databases_project_id_name_key";

/// [`proto::Engine`] as a `VARCHAR` column, in the same `snake_case` spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
	Postgres,
	Mysql,
	Mongodb,
	Redis,
}

impl From<EngineKind> for Engine {
	fn from(kind: EngineKind) -> Self {
		match kind {
			EngineKind::Postgres => Self::Postgres,
			EngineKind::Mysql => Self::Mysql,
			EngineKind::Mongodb => Self::Mongodb,
			EngineKind::Redis => Self::Redis,
		}
	}
}

/// Lifecycle state of a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum DatabaseStatus {
	/// Created or being (re)provisioned.
	Pending,
	Running,
	Stopped,
	/// A job failed or the container crashed; see `status_message`.
	Failed,
	/// A remove job is queued or running.
	Deleting,
}

/// Row from the `databases` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Database {
	pub id: DatabaseId,
	pub project_id: ProjectId,
	pub server_id: ServerId,
	pub name: String,
	pub engine: EngineKind,
	pub version: String,
	/// age-encrypted JSON of [`Credentials`].
	pub credentials: Vec<u8>,
	pub public_port: Option<i32>,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
	pub status: DatabaseStatus,
	pub status_message: Option<String>,
	pub container_id: Option<String>,
	pub backup_target_id: Option<BackupTargetId>,
	/// 5-field UTC cron expression.
	pub backup_schedule: Option<String>,
	pub backup_keep: i32,
	pub backup_next_run_at: Option<OffsetDateTime>,
}

impl Database {
	#[must_use]
	pub fn engine(&self) -> Engine {
		self.engine.into()
	}
}

/// What the engine container is configured with. Redis ignores `username`
/// and `database`; `MySQL` uses `password` for root as well.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Credentials {
	pub username: String,
	pub password: String,
	pub database: String,
}

impl Credentials {
	/// Fresh credentials with a random password.
	#[must_use]
	pub fn generate() -> Self {
		Self {
			username: "podkit".to_owned(),
			password: crypto::generate_token(24),
			database: "podkit".to_owned(),
		}
	}
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
	#[error("credentials decrypt failed: {0}")]
	Decrypt(#[from] crypto::age::AgeError),
	#[error("credentials are not valid json: {0}")]
	Json(#[from] serde_json::Error),
}

/// # Errors
/// [`CredentialsError`] when the stored blob cannot be decrypted or parsed.
pub fn decrypt_credentials(
	secrets: &SecretBox,
	database: &Database,
) -> Result<Credentials, CredentialsError> {
	let json = secrets.decrypt(&database.credentials)?;
	Ok(serde_json::from_str(&json)?)
}

/// Fields needed to insert a new database row.
pub struct NewDatabase {
	pub id: DatabaseId,
	pub project_id: ProjectId,
	pub server_id: ServerId,
	pub name: String,
	pub engine: EngineKind,
	pub version: String,
	pub credentials: Vec<u8>,
	pub public_port: Option<i32>,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
}

/// Full replacement of the editable settings.
pub struct SettingsUpdate {
	pub public_port: Option<i32>,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
	pub backup_target_id: Option<BackupTargetId>,
	pub backup_schedule: Option<String>,
	pub backup_keep: i32,
	pub backup_next_run_at: Option<OffsetDateTime>,
}

pub async fn insert(db: impl PgExecutor<'_>, new: NewDatabase) -> Result<Database, sqlx::Error> {
	sqlx::query_as!(
		Database,
		r#"
			INSERT INTO databases
				(id, project_id, server_id, name, engine, version, credentials, public_port,
				 memory_limit_mb, cpu_limit)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
			RETURNING
				id AS "id: DatabaseId", project_id AS "project_id: ProjectId",
				server_id AS "server_id: ServerId", name, engine AS "engine: EngineKind", version,
				credentials, public_port, memory_limit_mb, cpu_limit,
				status AS "status: DatabaseStatus", status_message, container_id,
				backup_target_id AS "backup_target_id: BackupTargetId", backup_schedule,
				backup_keep, backup_next_run_at
		"#,
		new.id as DatabaseId,
		new.project_id as ProjectId,
		new.server_id as ServerId,
		new.name,
		new.engine as EngineKind,
		new.version,
		new.credentials,
		new.public_port,
		new.memory_limit_mb,
		new.cpu_limit,
	)
	.fetch_one(db)
	.await
}

/// Lookup by id alone, with no team check. Only for jobs; handlers use
/// `scope::database`.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: DatabaseId,
) -> Result<Option<Database>, sqlx::Error> {
	sqlx::query_as!(
		Database,
		r#"
			SELECT
				id AS "id: DatabaseId", project_id AS "project_id: ProjectId",
				server_id AS "server_id: ServerId", name, engine AS "engine: EngineKind", version,
				credentials, public_port, memory_limit_mb, cpu_limit,
				status AS "status: DatabaseStatus", status_message, container_id,
				backup_target_id AS "backup_target_id: BackupTargetId", backup_schedule,
				backup_keep, backup_next_run_at
			FROM databases WHERE id = $1
		"#,
		id as DatabaseId,
	)
	.fetch_optional(db)
	.await
}

pub async fn list_by_project(
	db: impl PgExecutor<'_>,
	project: ProjectId,
) -> Result<Vec<Database>, sqlx::Error> {
	sqlx::query_as!(
		Database,
		r#"
			SELECT
				id AS "id: DatabaseId", project_id AS "project_id: ProjectId",
				server_id AS "server_id: ServerId", name, engine AS "engine: EngineKind", version,
				credentials, public_port, memory_limit_mb, cpu_limit,
				status AS "status: DatabaseStatus", status_message, container_id,
				backup_target_id AS "backup_target_id: BackupTargetId", backup_schedule,
				backup_keep, backup_next_run_at
			FROM databases WHERE project_id = $1 ORDER BY id
		"#,
		project as ProjectId,
	)
	.fetch_all(db)
	.await
}

/// The caller must already have resolved `id` through `scope::database`.
pub async fn update_settings(
	db: impl PgExecutor<'_>,
	id: DatabaseId,
	u: SettingsUpdate,
) -> Result<Database, sqlx::Error> {
	sqlx::query_as!(
		Database,
		r#"
			UPDATE databases
			SET public_port = $2, memory_limit_mb = $3, cpu_limit = $4, backup_target_id = $5,
				backup_schedule = $6, backup_keep = $7, backup_next_run_at = $8, updated_at = NOW()
			WHERE id = $1
			RETURNING
				id AS "id: DatabaseId", project_id AS "project_id: ProjectId",
				server_id AS "server_id: ServerId", name, engine AS "engine: EngineKind", version,
				credentials, public_port, memory_limit_mb, cpu_limit,
				status AS "status: DatabaseStatus", status_message, container_id,
				backup_target_id AS "backup_target_id: BackupTargetId", backup_schedule,
				backup_keep, backup_next_run_at
		"#,
		id as DatabaseId,
		u.public_port,
		u.memory_limit_mb,
		u.cpu_limit,
		u.backup_target_id as Option<BackupTargetId>,
		u.backup_schedule,
		u.backup_keep,
		u.backup_next_run_at,
	)
	.fetch_one(db)
	.await
}

pub async fn set_status(
	db: impl PgExecutor<'_>,
	id: DatabaseId,
	status: DatabaseStatus,
	message: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE databases SET status = $2, status_message = $3, updated_at = NOW() WHERE id = $1",
		id as DatabaseId,
		status as DatabaseStatus,
		message,
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, id: DatabaseId) -> Result<(), sqlx::Error> {
	sqlx::query!("DELETE FROM databases WHERE id = $1", id as DatabaseId)
		.execute(db)
		.await?;
	Ok(())
}

/// Queues a `kind` job for `database`. All of a database's jobs share one
/// lock key, so they run one at a time in the order they were queued.
pub async fn enqueue(
	state: &AppState,
	kind: JobKind,
	database: &Database,
) -> Result<(), ServerError> {
	jobs::enqueue(
		state,
		kind,
		database.server_id,
		database.id.0,
		format!("db:{}", database.id),
	)
	.await?;
	Ok(())
}
