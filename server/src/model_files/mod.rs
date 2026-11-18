//! Model files: Hugging Face weights downloaded into a podman volume on one
//! server. A row in `model_files` is the download and, once `ready`, the
//! weights; engines mount the volume, so one download serves several
//! deployments. Downloads run as `JobKind::ModelDownload` and resume.
//!
//! Routes live under `/api/v1/teams/{team}/model-files` and
//! `/api/v1/teams/{team}/huggingface-token`.

pub mod hf;
mod routes;
pub mod run;
pub mod service;
pub mod token;

use sqlx::PgExecutor;
use sqlx::types::Json;
use time::OffsetDateTime;

use crate::error::ServerError;
use crate::ids::{ModelFileId, ServerId, TeamId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;

pub use routes::router;

#[cfg(test)]
mod api_tests;

/// Lifecycle state of a model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ModelFileStatus {
	/// Waiting for a job slot.
	Queued,
	Downloading,
	/// Every selected file is on the volume.
	Ready,
	/// See `error`. A retry resumes where the download stopped.
	Failed,
}

/// Row from the `model_files` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ModelFile {
	pub id: ModelFileId,
	pub team_id: TeamId,
	pub server_id: ServerId,
	pub repo: String,
	pub revision: String,
	pub selection: Json<hf::Selection>,
	pub total_bytes: i64,
	pub downloaded_bytes: i64,
	pub status: ModelFileStatus,
	pub volume_name: String,
	pub error: Option<String>,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

const COLUMNS: &str = "id, team_id, server_id, repo, revision, selection, total_bytes, \
	downloaded_bytes, status, volume_name, error, created_at, updated_at";

/// Fields needed to insert a new model file.
pub struct NewModelFile {
	pub id: ModelFileId,
	pub team_id: TeamId,
	pub server_id: ServerId,
	pub repo: String,
	pub revision: String,
	pub selection: hf::Selection,
	pub total_bytes: i64,
}

/// Podman volume holding the files of model file `id`.
#[must_use]
pub fn volume_name(id: ModelFileId) -> String {
	format!("model-files-{id}")
}

pub async fn insert(db: impl PgExecutor<'_>, new: NewModelFile) -> Result<ModelFile, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"INSERT INTO model_files
			(id, team_id, server_id, repo, revision, selection, total_bytes, volume_name)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLUMNS}"
	)))
	.bind(new.id)
	.bind(new.team_id)
	.bind(new.server_id)
	.bind(new.repo)
	.bind(new.revision)
	.bind(Json(new.selection))
	.bind(new.total_bytes)
	.bind(volume_name(new.id))
	.fetch_one(db)
	.await
}

/// A model file of `team`; `None` for another team's too.
pub async fn find(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: ModelFileId,
) -> Result<Option<ModelFile>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM model_files WHERE id = $1 AND team_id = $2"
	)))
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await
}

/// Lookup by id alone, with no team check. Only for jobs; handlers use [`find`].
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: ModelFileId,
) -> Result<Option<ModelFile>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM model_files WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

pub async fn list(db: impl PgExecutor<'_>, team: TeamId) -> Result<Vec<ModelFile>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM model_files WHERE team_id = $1 ORDER BY id DESC"
	)))
	.bind(team)
	.fetch_all(db)
	.await
}

/// Whether the team already has a non-failed download of the same files to the same server.
pub async fn duplicate_exists(
	db: impl PgExecutor<'_>,
	team: TeamId,
	server: ServerId,
	repo: &str,
	revision: &str,
	selection: &hf::Selection,
) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM model_files
		WHERE team_id = $1 AND server_id = $2 AND repo = $3 AND revision = $4
			AND selection = $5 AND status <> 'failed')",
	)
	.bind(team)
	.bind(server)
	.bind(repo)
	.bind(revision)
	.bind(Json(selection))
	.fetch_one(db)
	.await
}

pub async fn delete(db: impl PgExecutor<'_>, id: ModelFileId) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM model_files WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

/// Puts a failed download back in the queue with a fresh size. The downloaded
/// byte count stays: the volume still holds those bytes.
pub async fn requeue(
	db: impl PgExecutor<'_>,
	id: ModelFileId,
	total_bytes: i64,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE model_files SET status = 'queued', error = NULL, total_bytes = $2,
			updated_at = NOW() WHERE id = $1",
	)
	.bind(id)
	.bind(total_bytes)
	.execute(db)
	.await?;
	Ok(())
}

/// Queues the download job of `file`. All of a model file's jobs share one lock key.
pub async fn enqueue(state: &AppState, file: &ModelFile) -> Result<(), ServerError> {
	jobs::enqueue(
		state,
		JobKind::ModelDownload,
		file.server_id,
		file.id.0,
		format!("model-file:{}", file.id),
	)
	.await?;
	Ok(())
}

/// Replaces `token` in `text` so it cannot reach a log line or an error.
#[must_use]
pub fn redact(text: &str, token: Option<&str>) -> String {
	match token.filter(|t| !t.is_empty()) {
		Some(token) => text.replace(token, "***"),
		None => text.to_owned(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn redact_replaces_every_occurrence() {
		assert_eq!(redact("a hf_x b hf_x", Some("hf_x")), "a *** b ***");
		assert_eq!(redact("nothing", Some("hf_x")), "nothing");
		assert_eq!(redact("keep", None), "keep");
		assert_eq!(redact("keep", Some("")), "keep");
	}

	#[test]
	fn volume_names_follow_the_id() {
		assert_eq!(volume_name(ModelFileId(42)), "model-files-42");
	}
}
