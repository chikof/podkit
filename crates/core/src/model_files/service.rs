//! What the model-file routes do, free of HTTP: each function authorizes,
//! scopes to the team, validates, and only then touches the database or the
//! agent. The web UI calls these too.

use axum::http::Method;

use super::hf::{self, Selection};
use super::{ModelFile, ModelFileStatus, NewModelFile};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{JobId, ModelFileId, ServerId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::servers::Server;
use crate::state::AppState;
use crate::{model_files, scope};

const MIB: u64 = 1 << 20;

/// A request to download Hugging Face files.
pub struct CreateFile {
	pub server_id: ServerId,
	/// `owner/name`.
	pub repo: String,
	/// Branch, tag or commit; `main` when absent.
	pub revision: Option<String>,
	pub selection: Selection,
}

/// Whether the download is queued or running.
#[must_use]
pub fn active(status: ModelFileStatus) -> bool {
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
/// queues the download.
///
/// # Errors
/// `Forbidden` without `create model_file`; `NotFound` for another team's
/// server; `Validation` or `Conflict` for a request that cannot work.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	req: CreateFile,
) -> AppResult<ModelFile> {
	authorize(&s.db, user, team, Action::Create, Resource::ModelFile).await?;
	let server = scope::server(&s.db, team, req.server_id).await?;
	crate::moves::ensure_open(&s.db, &server).await?;

	let repo = req.repo.trim().to_owned();
	let revision = req.revision.unwrap_or_else(|| "main".to_owned());
	if !hf::valid_repo(&repo) {
		return Err(ServerError::Validation("invalid repo".to_owned()));
	}
	if !hf::valid_revision(&revision) {
		return Err(ServerError::Validation("invalid revision".to_owned()));
	}
	let selection = req.selection;
	selection.validate()?;
	if model_files::duplicate_exists(&s.db, team, server.id, &repo, &revision, &selection).await? {
		return Err(ServerError::Conflict(
			"those files are already downloaded or downloading on this server".to_owned(),
		));
	}

	let plan = preflight(s, team, &server, (&repo, &revision, &selection), 0).await?;

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
	if let Err(e) = model_files::enqueue(s, &file).await {
		model_files::delete(&s.db, file.id).await?;
		return Err(e);
	}
	log(
		s,
		user,
		team,
		&file,
		"model_file.create",
		Diff::new()
			.set("repo", file.repo.clone())
			.set("revision", file.revision.clone())
			.set("server_id", file.server_id.0)
			.set("total_bytes", file.total_bytes),
	)
	.await;
	Ok(file)
}

/// Records an action on a model file.
async fn log(s: &AppState, user: UserId, team: TeamId, file: &ModelFile, action: &str, diff: Diff) {
	audit::log(
		&s.db,
		user,
		team,
		action,
		Target::new("model_file", file.id).named(&file.repo),
		diff,
	)
	.await;
}

/// The team's model files, newest first.
///
/// # Errors
/// `Forbidden` without `read model_file`.
pub async fn list(s: &AppState, user: UserId, team: TeamId) -> AppResult<Vec<ModelFile>> {
	authorize(&s.db, user, team, Action::Read, Resource::ModelFile).await?;
	Ok(model_files::list(&s.db, team).await?)
}

/// One model file with its progress.
///
/// # Errors
/// `Forbidden` without `read model_file`; `NotFound` for another team's file.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: ModelFileId,
) -> AppResult<ModelFile> {
	authorize(&s.db, user, team, Action::Read, Resource::ModelFile).await?;
	scoped(s, team, id).await
}

/// The job that downloads `id`, newest first, for its log.
///
/// # Errors
/// As [`get`].
pub async fn latest_job(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: ModelFileId,
) -> AppResult<Option<JobId>> {
	let file = get(s, user, team, id).await?;
	Ok(jobs::latest_for(&s.db, JobKind::ModelDownload, file.id.0)
		.await?
		.map(|job| job.id))
}

/// Queues a failed download again. The volume keeps what was downloaded, so it
/// resumes. The size is checked again (the token or the repo may have changed).
///
/// # Errors
/// `Forbidden` without `update model_file`; `Conflict` unless it failed.
pub async fn retry(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: ModelFileId,
) -> AppResult<ModelFile> {
	authorize(&s.db, user, team, Action::Update, Resource::ModelFile).await?;
	let file = scoped(s, team, id).await?;
	if file.status != ModelFileStatus::Failed {
		return Err(ServerError::Conflict(
			"only a failed download can be retried".to_owned(),
		));
	}
	let server = scope::server(&s.db, team, file.server_id).await?;
	let have = u64::try_from(file.downloaded_bytes).unwrap_or(0);
	let plan = preflight(
		s,
		team,
		&server,
		(&file.repo, &file.revision, &file.selection),
		have,
	)
	.await?;

	model_files::requeue(&s.db, file.id, to_i64(plan.total_bytes)?).await?;
	model_files::enqueue(s, &file).await.inspect_err(|_| {
		tracing::error!(model_file = file.id.0, "retry could not queue its job");
	})?;
	log(s, user, team, &file, "model_file.retry", Diff::new()).await;
	scoped(s, team, id).await
}

/// Stops a queued or running download. The volume is kept; retry resumes.
///
/// # Errors
/// `Forbidden` without `update model_file`; `Conflict` when nothing is downloading.
pub async fn cancel(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: ModelFileId,
) -> AppResult<ModelFile> {
	authorize(&s.db, user, team, Action::Update, Resource::ModelFile).await?;
	let file = scoped(s, team, id).await?;
	if !active(file.status) {
		return Err(ServerError::Conflict("nothing is downloading".to_owned()));
	}
	if let Some(job) = jobs::latest_for(&s.db, JobKind::ModelDownload, file.id.0).await? {
		jobs::worker::cancel(s, job.id).await?;
	}
	log(s, user, team, &file, "model_file.cancel", Diff::new()).await;
	scoped(s, team, id).await
}

/// Removes the volume and its files from the server, then the row. A running
/// download must be cancelled first, and a volume still mounted by an engine
/// is refused.
///
/// # Errors
/// `Forbidden` without `delete model_file`; `Conflict` while the files are in use.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: ModelFileId,
) -> AppResult<ModelFileId> {
	authorize(&s.db, user, team, Action::Delete, Resource::ModelFile).await?;
	let file = scoped(s, team, id).await?;
	if active(file.status) {
		return Err(ServerError::Conflict(
			"the download is still running; cancel it first".to_owned(),
		));
	}
	if let Some(model) = crate::models::name_using_file(&s.db, file.id).await? {
		return Err(ServerError::Conflict(format!(
			"the model {model} serves these files; remove it first"
		)));
	}
	let server = scope::server(&s.db, team, file.server_id).await?;
	s.agents
		.client(s, &server)
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
	log(
		s,
		user,
		team,
		&file,
		"model_file.delete",
		Diff::new().set("repo", file.repo.clone()),
	)
	.await;
	Ok(file.id)
}
