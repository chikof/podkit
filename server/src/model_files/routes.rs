//! HTTP handlers of the model-files slice. Each one authorizes, scopes to the
//! team, validates, and only then touches the database or the agent.

use axum::extract::{Path, State};
use axum::http::Method;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::hf::{self, Selection};
use super::{ModelFile, ModelFileStatus, NewModelFile};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ModelFileId, ServerId, TeamId};
use crate::jobs::{self, JobKind};
use crate::servers::Server;
use crate::state::AppState;
use crate::{model_files, scope};

const MIB: u64 = 1 << 20;

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

fn active(status: ModelFileStatus) -> bool {
	matches!(
		status,
		ModelFileStatus::Queued | ModelFileStatus::Downloading
	)
}

async fn scoped(s: &AppState, team: TeamId, id: ModelFileId) -> AppResult<ModelFile> {
	model_files::find(&s.db, team, id)
		.await?
		.ok_or(ServerError::ModelFileNotFound)
}

/// Free bytes on the server's smallest reported disk (the Podman storage and
/// the agent data dir may differ).
fn free_bytes(server: &Server) -> AppResult<u64> {
	server
		.hardware
		.as_ref()
		.and_then(|h| h.disks.iter().map(|d| d.free_mb).min())
		.map(|mb| mb.saturating_mul(MIB))
		.ok_or_else(|| {
			ServerError::Conflict(
				"the server has not reported its disk space yet; try again shortly".to_owned(),
			)
		})
}

/// Lists the files on Hugging Face and checks that what is still missing fits
/// on the server. Nothing is queued or stored before this passes.
async fn preflight(
	s: &AppState,
	team: TeamId,
	server: &Server,
	file: (&str, &str, &Selection),
	already_downloaded: u64,
) -> AppResult<hf::Plan> {
	let (repo, revision, selection) = file;
	let token = model_files::token::fetch(&s.db, &s.secrets, team).await?;
	let plan = hf::plan(
		&s.http,
		&s.config.hf_endpoint,
		token.as_deref(),
		repo,
		revision,
		selection,
	)
	.await?;
	let missing = plan.total_bytes.saturating_sub(already_downloaded);
	hf::check_fit(missing, free_bytes(server)?)?;
	Ok(plan)
}

fn to_i64(bytes: u64) -> AppResult<i64> {
	i64::try_from(bytes)
		.map_err(|_| ServerError::Validation("the selection is too large".to_owned()))
}

/// Checks the size against Hugging Face and the server's free disk, then
/// queues the download. Caller must hold `create model_file` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateRequest>,
) -> AppResult<Json<ModelFileResponse>> {
	authorize(&s.db, user.id, team, Action::Create, Resource::ModelFile).await?;
	let server = scope::server(&s.db, team, body.server_id).await?;

	let repo = body.repo.trim().to_owned();
	let revision = body.revision.unwrap_or_else(|| "main".to_owned());
	if !hf::valid_repo(&repo) {
		return Err(ServerError::Validation("invalid repo".to_owned()));
	}
	if !hf::valid_revision(&revision) {
		return Err(ServerError::Validation("invalid revision".to_owned()));
	}
	let selection = Selection {
		include: body.include,
		files: body.files,
	};
	selection.validate()?;
	if model_files::duplicate_exists(&s.db, team, server.id, &repo, &revision, &selection).await? {
		return Err(ServerError::Conflict(
			"those files are already downloaded or downloading on this server".to_owned(),
		));
	}

	let plan = preflight(&s, team, &server, (&repo, &revision, &selection), 0).await?;

	let file = model_files::insert(
		&s.db,
		NewModelFile {
			id: ModelFileId::generate(),
			team_id: team,
			server_id: server.id,
			repo,
			revision,
			selection,
			total_bytes: to_i64(plan.total_bytes)?,
		},
	)
	.await?;
	if let Err(e) = model_files::enqueue(&s, &file).await {
		model_files::delete(&s.db, file.id).await?;
		return Err(e);
	}
	Ok(Json(file.into()))
}

/// Lists the team's model files, newest first; caller must hold `read model_file`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<ModelFileResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::ModelFile).await?;
	let files = model_files::list(&s.db, team).await?;
	Ok(Json(files.into_iter().map(Into::into).collect()))
}

/// One model file with its progress; caller must hold `read model_file`.
async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<ModelFileResponse>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::ModelFile).await?;
	Ok(Json(scoped(&s, team, id).await?.into()))
}

/// Queues a failed download again. The volume keeps what was downloaded, so it
/// resumes. The size is checked again (the token or the repo may have changed).
/// Caller must hold `update model_file`.
async fn retry(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<ModelFileResponse>> {
	authorize(&s.db, user.id, team, Action::Update, Resource::ModelFile).await?;
	let file = scoped(&s, team, id).await?;
	if file.status != ModelFileStatus::Failed {
		return Err(ServerError::Conflict(
			"only a failed download can be retried".to_owned(),
		));
	}
	let server = scope::server(&s.db, team, file.server_id).await?;
	let have = u64::try_from(file.downloaded_bytes).unwrap_or(0);
	let plan = preflight(
		&s,
		team,
		&server,
		(&file.repo, &file.revision, &file.selection),
		have,
	)
	.await?;

	model_files::requeue(&s.db, file.id, to_i64(plan.total_bytes)?).await?;
	model_files::enqueue(&s, &file).await.inspect_err(|_| {
		tracing::error!(model_file = file.id.0, "retry could not queue its job");
	})?;
	Ok(Json(scoped(&s, team, id).await?.into()))
}

/// Stops a queued or running download. The volume is kept; retry resumes.
/// Caller must hold `update model_file`.
async fn cancel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<ModelFileResponse>> {
	authorize(&s.db, user.id, team, Action::Update, Resource::ModelFile).await?;
	let file = scoped(&s, team, id).await?;
	if !active(file.status) {
		return Err(ServerError::Conflict("nothing is downloading".to_owned()));
	}
	if let Some(job) = jobs::latest_for(&s.db, JobKind::ModelDownload, file.id.0).await? {
		jobs::worker::cancel(&s, job.id).await?;
	}
	Ok(Json(scoped(&s, team, id).await?.into()))
}

/// Removes the volume and its files from the server, then the row. A running
/// download must be cancelled first, and a volume still mounted by an engine
/// is refused. Caller must hold `delete model_file`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ModelFileId)>,
) -> AppResult<Json<DeleteResponse>> {
	authorize(&s.db, user.id, team, Action::Delete, Resource::ModelFile).await?;
	let file = scoped(&s, team, id).await?;
	if active(file.status) {
		return Err(ServerError::Conflict(
			"the download is still running; cancel it first".to_owned(),
		));
	}
	let server = scope::server(&s.db, team, file.server_id).await?;
	s.agents
		.client(&s, &server)
		.await?
		.send(Method::DELETE, &format!("/v1/volumes/{}", file.volume_name))
		.await
		.map_err(|e| match e {
			ServerError::Conflict(_) => {
				ServerError::Conflict("the volume is in use; stop what uses it first".to_owned())
			}
			e => e,
		})?;
	model_files::delete(&s.db, file.id).await?;
	Ok(Json(DeleteResponse { id: file.id }))
}
