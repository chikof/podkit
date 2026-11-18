//! HTTP handlers of the model-files slice. They only translate: the rules live
//! in [`super::service`].

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::hf::Selection;
use super::service::{self, CreateFile};
use super::{ModelFile, ModelFileStatus};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ModelFileId, ServerId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{model_file_id}", get(show).delete(remove))
		.route("/{model_file_id}/retry", post(retry))
		.route("/{model_file_id}/cancel", post(cancel))
}

#[derive(Deserialize)]
struct CreateRequest {
	server_id: ServerId,
	/// Hugging Face repo id, `owner/name`.
	repo: String,
	/// Branch, tag or commit; `main` when absent.
	revision: Option<String>,
	/// Glob patterns, like `*Q4_K_M.gguf`.
	#[serde(default)]
	include: Vec<String>,
	/// Exact file paths.
	#[serde(default)]
	files: Vec<String>,
}

/// The HF token is not here and never was: it is not part of the row.
#[derive(Serialize)]
struct ModelFileResponse {
	id: ModelFileId,
	server_id: ServerId,
	repo: String,
	revision: String,
	selection: Selection,
	total_bytes: i64,
	downloaded_bytes: i64,
	status: ModelFileStatus,
	volume_name: String,
	error: Option<String>,
	created_at: String,
	updated_at: String,
}

impl From<ModelFile> for ModelFileResponse {
	fn from(f: ModelFile) -> Self {
		Self {
			id: f.id,
			server_id: f.server_id,
			repo: f.repo,
			revision: f.revision,
			selection: f.selection.0,
			total_bytes: f.total_bytes,
			downloaded_bytes: f.downloaded_bytes,
			status: f.status,
			volume_name: f.volume_name,
			error: f.error,
			created_at: f.created_at.to_string(),
			updated_at: f.updated_at.to_string(),
		}
	}
}

#[derive(Serialize)]
struct DeleteResponse {
	id: ModelFileId,
}

/// Checks the size against Hugging Face and the server's free disk, then
/// queues the download. Caller must hold `create model_file` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateRequest>,
) -> AppResult<Json<ModelFileResponse>> {
	let file = service::create(
		&s,
		user.id,
		team,
		CreateFile {
			server_id: body.server_id,
			repo: body.repo,
			revision: body.revision,
			selection: Selection {
				include: body.include,
				files: body.files,
			},
		},
	)
	.await?;
	Ok(Json(file.into()))
}

/// Lists the team's model files, newest first; caller must hold `read model_file`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ModelFileResponse>>> {
	let files = service::list(&s, user.id, team).await?;
	Ok(Json(files.into_iter().map(Into::into).collect()))
}

/// One model file with its progress; caller must hold `read model_file`.
async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<ModelFileResponse>> {
	Ok(Json(service::get(&s, user.id, team, id).await?.into()))
}

/// Queues a failed download again; caller must hold `update model_file`.
async fn retry(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<ModelFileResponse>> {
	Ok(Json(service::retry(&s, user.id, team, id).await?.into()))
}

/// Stops a queued or running download; caller must hold `update model_file`.
async fn cancel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<ModelFileResponse>> {
	Ok(Json(service::cancel(&s, user.id, team, id).await?.into()))
}

/// Removes the files and the row; caller must hold `delete model_file`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<DeleteResponse>> {
	let id = service::remove(&s, user.id, team, id).await?;
	Ok(Json(DeleteResponse { id }))
}
