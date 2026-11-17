//! Models: an inference engine container serving the files of one model
//! file download, reachable on the `podkit` network as `model-<slug>` with an
//! OpenAI-compatible API. Rows live in the `models` table; the container is
//! run by the `ModelStart`, `ModelStop` and `ModelRemove` jobs.
//!
//! Routes live under `/api/v1/teams/{team}/projects/{project}/models`.

pub mod engines;
mod routes;
pub mod run;
pub mod service;

#[cfg(test)]
mod api_tests;

use sqlx::PgExecutor;
use sqlx::types::Json;
use time::OffsetDateTime;

use crate::error::ServerError;
use crate::ids::{ModelFileId, ModelId, ProjectId, ServerId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;

pub use engines::EngineKind;
pub use routes::router;

/// Unique constraint on `models (project_id, name)`.
pub const NAME_CONSTRAINT: &str = "models_project_id_name_key";
/// Unique constraint on `models (slug)`.
pub const SLUG_CONSTRAINT: &str = "models_slug_key";

/// Lifecycle state of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
	/// The container is being deployed or the engine is still loading the weights.
	Starting,
	/// The engine answered its readiness check.
	Running,
	Stopped,
	/// A job failed or the engine never became ready; see `error`.
	Failed,
	/// A remove job is queued or running.
	Removing,
}

/// Row from the `models` table.
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(
	clippy::struct_field_names,
	reason = "columns are named after the model files and the model's path, as the table is"
)]
pub struct Model {
	pub id: ModelId,
	pub project_id: ProjectId,
	pub server_id: ServerId,
	pub model_files_id: ModelFileId,
	pub name: String,
	pub slug: String,
	pub engine: EngineKind,
	pub image: String,
	pub port: i32,
	pub model_path: String,
	pub served_name: String,
	pub context_size: i32,
	pub gpu: bool,
	pub extra_args: Json<Vec<String>>,
	pub memory_limit_mb: Option<i32>,
	pub status: ModelStatus,
	pub error: Option<String>,
	pub container_id: Option<String>,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

impl Model {
	/// The name other containers on the server's `podkit` network use.
	#[must_use]
	pub fn alias(&self) -> String {
		alias(&self.slug)
	}

	/// The service name the agent knows the container by.
	#[must_use]
	pub fn service(&self) -> String {
		format!("model-{}", self.id)
	}

	/// Volume holding what the engine keeps for itself (Ollama's store).
	#[must_use]
	pub fn state_volume(&self) -> String {
		format!("podkit-model-{}-state", self.id)
	}

	/// Where other containers reach the OpenAI-compatible API.
	#[must_use]
	pub fn internal_url(&self) -> String {
		format!("http://{}:{}/v1", self.alias(), self.port)
	}
}

#[must_use]
pub fn alias(slug: &str) -> String {
	format!("model-{slug}")
}

const COLUMNS: &str = "id, project_id, server_id, model_files_id, name, slug, engine, image, port, \
	model_path, served_name, context_size, gpu, extra_args, memory_limit_mb, status, error, \
	container_id, created_at, updated_at";

/// Fields needed to insert a new model.
pub struct NewModel {
	pub id: ModelId,
	pub project_id: ProjectId,
	pub server_id: ServerId,
	pub model_files_id: ModelFileId,
	pub name: String,
	pub slug: String,
	pub engine: EngineKind,
	pub image: String,
	pub port: i32,
	pub model_path: String,
	pub served_name: String,
	pub context_size: i32,
	pub gpu: bool,
	pub extra_args: Vec<String>,
	pub memory_limit_mb: Option<i32>,
}

pub async fn insert(db: impl PgExecutor<'_>, new: NewModel) -> Result<Model, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"INSERT INTO models
			(id, project_id, server_id, model_files_id, name, slug, engine, image, port,
			 model_path, served_name, context_size, gpu, extra_args, memory_limit_mb)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
		RETURNING {COLUMNS}"
	)))
	.bind(new.id)
	.bind(new.project_id)
	.bind(new.server_id)
	.bind(new.model_files_id)
	.bind(new.name)
	.bind(new.slug)
	.bind(new.engine)
	.bind(new.image)
	.bind(new.port)
	.bind(new.model_path)
	.bind(new.served_name)
	.bind(new.context_size)
	.bind(new.gpu)
	.bind(Json(new.extra_args))
	.bind(new.memory_limit_mb)
	.fetch_one(db)
	.await
}

/// Lookup by id alone, with no team check. Only for jobs; handlers use
/// `scope::model`.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: ModelId,
) -> Result<Option<Model>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM models WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// A model of `project`, joined up to its team so another team's id is a miss.
pub async fn find_scoped(
	db: impl PgExecutor<'_>,
	team: crate::ids::TeamId,
	project: ProjectId,
	id: ModelId,
) -> Result<Option<Model>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {} FROM models m JOIN projects p ON p.id = m.project_id
		WHERE m.id = $3 AND m.project_id = $2 AND p.team_id = $1",
		COLUMNS
			.split(", ")
			.map(|c| format!("m.{c}"))
			.collect::<Vec<_>>()
			.join(", ")
	)))
	.bind(team)
	.bind(project)
	.bind(id)
	.fetch_optional(db)
	.await
}

pub async fn list_by_project(
	db: impl PgExecutor<'_>,
	project: ProjectId,
) -> Result<Vec<Model>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM models WHERE project_id = $1 ORDER BY id"
	)))
	.bind(project)
	.fetch_all(db)
	.await
}

/// The name of a model that serves `file`, if any.
pub async fn name_using_file(
	db: impl PgExecutor<'_>,
	file: ModelFileId,
) -> Result<Option<String>, sqlx::Error> {
	sqlx::query_scalar("SELECT name FROM models WHERE model_files_id = $1 ORDER BY id LIMIT 1")
		.bind(file)
		.fetch_optional(db)
		.await
}

/// Sets the status and error. `Running` and `Stopped` clear the error.
pub async fn set_status(
	db: impl PgExecutor<'_>,
	id: ModelId,
	status: ModelStatus,
	error: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE models SET status = $2, error = $3, updated_at = NOW() WHERE id = $1")
		.bind(id)
		.bind(status)
		.bind(error)
		.execute(db)
		.await?;
	Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, id: ModelId) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM models WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

/// Queues a `kind` job for `model`. All of a model's jobs share one lock key,
/// so they run one at a time in the order they were queued.
pub async fn enqueue(state: &AppState, kind: JobKind, model: &Model) -> Result<(), ServerError> {
	jobs::enqueue(
		state,
		kind,
		model.server_id,
		model.id.0,
		format!("model:{}", model.id),
	)
	.await?;
	Ok(())
}
