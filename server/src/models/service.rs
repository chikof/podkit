//! What the model routes do, free of HTTP: each function authorizes, scopes
//! to the team, validates, and only then touches the database or the agent.
//! Creating a model refuses what cannot work, with the numbers, before a row
//! or a job exists.

use axum::http::Method;
use futures_util::stream::BoxStream;
use proto::{Event, GpuVendor};

use super::engines::{self, EngineKind, EngineSpec, GpuNeed};
use super::{Model, ModelStatus, NAME_CONSTRAINT, NewModel, SLUG_CONSTRAINT};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ModelFileId, ModelId, ProjectId, TeamId, UserId};
use crate::jobs::JobKind;
use crate::model_files::{self, ModelFileStatus};
use crate::servers::{self, Server};
use crate::state::AppState;
use crate::{db, models, scope};

const MIB: u64 = 1 << 20;
const DEFAULT_CONTEXT: u32 = 8192;
const MIN_CONTEXT: u32 = 256;
const MAX_CONTEXT: u32 = 1_048_576;
/// Runtime buffers beyond the weights, as a percentage of them.
const BUFFER_PERCENT: u64 = 15;
/// Fixed cost of the engine process, CUDA context and scratch space.
const BASE_OVERHEAD_MB: u64 = 512;
/// Rough KV-cache cost of one token of context, for models up to a few billion parameters.
const KV_KB_PER_TOKEN: u64 = 128;
const MAX_TAIL: u32 = 5000;

/// A request to create a model.
pub struct CreateModel {
	pub name: String,
	pub model_files_id: ModelFileId,
	pub engine: EngineKind,
	/// The model name clients send; the slug when absent.
	pub served_name: Option<String>,
	/// File or directory inside the download; worked out from the files when absent.
	pub model_path: Option<String>,
	pub context_size: Option<u32>,
	/// Whether to run on a GPU; the engine's default when absent.
	pub gpu: Option<bool>,
	pub extra_args: Vec<String>,
	/// The image of a [`EngineKind::Custom`] engine.
	pub image: Option<String>,
	/// The port of a [`EngineKind::Custom`] engine.
	pub port: Option<u16>,
	pub memory_limit_mb: Option<i32>,
}

/// What a model is expected to need, in MB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
	pub weights: u64,
	pub context: u64,
	pub overhead: u64,
}

impl Estimate {
	#[must_use]
	pub fn total_mb(&self) -> u64 {
		self.weights + self.context + self.overhead
	}
}

/// A rough memory estimate from the weights' size and the context length: the
/// weights, a KV cache that grows with the context, and fixed runtime costs.
/// Meant to catch a model that cannot fit, not to size one that barely does.
#[must_use]
pub fn estimate(weights_bytes: u64, context: u32) -> Estimate {
	let weights = weights_bytes.div_ceil(MIB);
	Estimate {
		weights,
		context: u64::from(context) * KV_KB_PER_TOKEN / 1024,
		overhead: BASE_OVERHEAD_MB + weights * BUFFER_PERCENT / 100,
	}
}

/// Refuses the model when the estimate exceeds what the server reports, with
/// both numbers in the message. `vendor` is `Some` for a model on a GPU.
///
/// # Errors
/// `Conflict` when the server has no inventory yet, `Validation` when it does not fit.
pub fn check_fit(
	engine: &EngineSpec,
	server: &Server,
	vendor: Option<GpuVendor>,
	estimate: &Estimate,
	context: u32,
) -> AppResult<()> {
	let Some(hardware) = server.hardware.as_ref() else {
		return Err(ServerError::Conflict(
			"the server has not reported its hardware yet; try again shortly".to_owned(),
		));
	};
	let (have, what) = match vendor {
		Some(vendor) => (servers::vram_mb(server, vendor), "GPU memory"),
		None => (hardware.memory.total_mb, "RAM"),
	};
	if estimate.total_mb() > have {
		return Err(ServerError::Validation(format!(
			"{} needs about {} MB ({} MB of weights, {} MB for a {context}-token context and {} MB \
			of runtime overhead) but {} has {have} MB of {what}",
			engine.label,
			estimate.total_mb(),
			estimate.weights,
			estimate.context,
			estimate.overhead,
			server.name
		)));
	}
	Ok(())
}

/// Whether the model runs on a GPU and which vendor's, or why it cannot.
///
/// # Errors
/// `Validation` for a GPU request the engine or the server cannot meet.
pub fn place(
	engine: &EngineSpec,
	server: &Server,
	requested: Option<bool>,
) -> AppResult<Option<GpuVendor>> {
	let vendors = servers::gpu_vendors(server);
	let wanted = match (engine.gpu, requested) {
		(GpuNeed::Never, Some(true)) => {
			return Err(ServerError::Validation(format!(
				"{} runs on the CPU only; pick a GPU engine to use the GPU",
				engine.label
			)));
		}
		(GpuNeed::Never, _) => false,
		(GpuNeed::Required, Some(false)) => {
			return Err(ServerError::Validation(format!(
				"{} needs a GPU and cannot run without one",
				engine.label
			)));
		}
		(GpuNeed::Required, _) => true,
		(GpuNeed::Optional, Some(wanted)) => wanted,
		(GpuNeed::Optional, None) => servers::gpu_capable(server),
	};
	if !wanted {
		return Ok(None);
	}
	let vendor = engine
		.gpu_vendor
		.or_else(|| vendors.first().copied())
		.unwrap_or(GpuVendor::Nvidia);
	if vendors.contains(&vendor) {
		return Ok(Some(vendor));
	}
	let name = match vendor {
		GpuVendor::Nvidia => "an NVIDIA GPU with a CDI spec",
		GpuVendor::Amd => "a discrete AMD GPU",
	};
	Err(ServerError::Validation(format!(
		"{} needs {name}, but {}",
		engine.label,
		servers::gpu_problem(server, vendor)
	)))
}

/// `Qwen3 4B (Q4)` becomes `qwen3-4b-q4`.
#[must_use]
pub fn slugify(name: &str) -> String {
	let mut slug = String::new();
	for c in name.chars() {
		if c.is_ascii_alphanumeric() {
			slug.push(c.to_ascii_lowercase());
		} else if !slug.is_empty() && !slug.ends_with('-') {
			slug.push('-');
		}
	}
	let slug = slug.trim_end_matches('-');
	let slug: String = slug.chars().take(40).collect();
	let slug = slug.trim_end_matches('-');
	if slug.is_empty() {
		"model".to_owned()
	} else {
		slug.to_owned()
	}
}

fn validate_name(name: &str) -> AppResult<()> {
	if name.trim().is_empty() || name.len() > 60 {
		return Err(ServerError::Validation(
			"name must be 1 to 60 characters".to_owned(),
		));
	}
	Ok(())
}

fn validate_served_name(name: &str) -> AppResult<()> {
	let ok = !name.is_empty()
		&& name.len() <= 100
		&& !name.starts_with(['-', '.', '/'])
		&& name
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'));
	if ok {
		Ok(())
	} else {
		Err(ServerError::Validation(
			"served_name must be 1 to 100 letters, digits or . _ - : /, not starting with - . or /"
				.to_owned(),
		))
	}
}

fn validate_image(image: &str) -> AppResult<()> {
	let ok = !image.is_empty()
		&& image.len() <= 255
		&& !image.starts_with(['-', '/'])
		&& !image.chars().any(|c| c.is_whitespace() || c.is_control());
	if ok {
		Ok(())
	} else {
		Err(ServerError::Validation(
			"image must be a reference like registry.example.com/team/engine:1.2, without spaces"
				.to_owned(),
		))
	}
}

/// The container port: the template's, or the caller's for a custom image.
fn custom_port(req: &CreateModel, engine: &EngineSpec) -> AppResult<u16> {
	let custom = req.engine == EngineKind::Custom;
	if custom != req.image.is_some() {
		return Err(ServerError::Validation(if custom {
			"a custom engine needs an image".to_owned()
		} else {
			"image is only for the custom engine".to_owned()
		}));
	}
	if !custom && req.port.is_some() {
		return Err(ServerError::Validation(
			"port is only for the custom engine".to_owned(),
		));
	}
	if let Some(image) = &req.image {
		validate_image(image)?;
	}
	match req.port.unwrap_or(engine.port) {
		0 => Err(ServerError::Validation(
			"port must be 1 to 65535".to_owned(),
		)),
		port => Ok(port),
	}
}

/// The context size, after checking it and the memory limit.
fn checked_context(req: &CreateModel) -> AppResult<u32> {
	let context = req.context_size.unwrap_or(DEFAULT_CONTEXT);
	if !(MIN_CONTEXT..=MAX_CONTEXT).contains(&context) {
		return Err(ServerError::Validation(format!(
			"context_size must be between {MIN_CONTEXT} and {MAX_CONTEXT}"
		)));
	}
	if req
		.memory_limit_mb
		.is_some_and(|mb| !(256..=4_194_304).contains(&mb))
	{
		return Err(ServerError::Validation(
			"memory_limit_mb must be between 256 and 4194304".to_owned(),
		));
	}
	Ok(context)
}

fn unique_conflict(e: sqlx::Error) -> ServerError {
	if db::is_unique_violation(&e, NAME_CONSTRAINT) {
		ServerError::Conflict("a model with this name already exists in this project".to_owned())
	} else {
		e.into()
	}
}

/// Creates the row and queues the first start. Poll the model until `status`
/// leaves `starting`, or follow the job log.
///
/// # Errors
/// `Forbidden` without `create model`; `NotFound` for another team's project
/// or files; `Validation` or `Conflict` for a request that cannot work.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	req: CreateModel,
) -> AppResult<Model> {
	authorize(&s.db, user, team, Action::Create, Resource::Model).await?;
	scope::project(&s.db, team, project).await?;
	let file = model_files::find(&s.db, team, req.model_files_id)
		.await?
		.ok_or(ServerError::ModelFileNotFound)?;
	let server = scope::server(&s.db, team, file.server_id).await?;

	validate_name(&req.name)?;
	let engine = engines::spec(req.engine);
	if file.status != ModelFileStatus::Ready {
		return Err(ServerError::Conflict(format!(
			"the model files are {}; wait until they are ready",
			serde_json::to_value(file.status)
				.ok()
				.and_then(|v| v.as_str().map(str::to_owned))
				.unwrap_or_default()
		)));
	}

	let port = custom_port(&req, engine)?;

	let (model_path, _format) =
		engines::resolve_model(engine, &file.selection, req.model_path.as_deref())
			.map_err(ServerError::Validation)?;
	engine
		.validate_extra_args(&req.extra_args)
		.map_err(ServerError::Validation)?;
	let context = checked_context(&req)?;
	let vendor = place(engine, &server, req.gpu)?;
	let weights = u64::try_from(file.total_bytes).unwrap_or(0);
	check_fit(
		engine,
		&server,
		vendor,
		&estimate(weights, context),
		context,
	)?;

	let base = slugify(&req.name);
	if let Some(served) = &req.served_name {
		validate_served_name(served)?;
	}
	let id = ModelId::generate();
	let mut model = None;
	for attempt in 0..5 {
		// The alias is one DNS name per server, shared by every team on it.
		let slug = if attempt == 0 {
			base.clone()
		} else {
			format!("{base}-{:04}", crypto::generate_id().rem_euclid(10_000))
		};
		let new = NewModel {
			id,
			project_id: project,
			server_id: server.id,
			model_files_id: file.id,
			name: req.name.clone(),
			served_name: req.served_name.clone().unwrap_or_else(|| slug.clone()),
			slug,
			engine: req.engine,
			image: engine.image(vendor.is_some(), req.image.as_deref()),
			port: i32::from(port),
			model_path: model_path.clone(),
			context_size: i32::try_from(context).unwrap_or(i32::MAX),
			gpu: vendor.is_some(),
			extra_args: req.extra_args.clone(),
			memory_limit_mb: req.memory_limit_mb,
		};
		match models::insert(&s.db, new).await {
			Ok(row) => {
				model = Some(row);
				break;
			}
			Err(e) if db::is_unique_violation(&e, SLUG_CONSTRAINT) => {}
			Err(e) => return Err(unique_conflict(e)),
		}
	}
	let model = model.ok_or_else(|| {
		ServerError::Conflict("could not find a free internal name; try another name".to_owned())
	})?;

	queue(s, JobKind::ModelStart, &model).await?;
	Ok(model)
}

/// Queues `kind`; when that fails the model is marked failed so it does not
/// sit in `starting` for good.
async fn queue(s: &AppState, kind: JobKind, model: &Model) -> AppResult<()> {
	if let Err(e) = models::enqueue(s, kind, model).await {
		if let Err(mark) = models::set_status(
			&s.db,
			model.id,
			ModelStatus::Failed,
			Some("failed to queue the job"),
		)
		.await
		{
			tracing::warn!(
				model = model.id.0,
				"failed to mark unqueued model failed: {mark}"
			);
		}
		return Err(e);
	}
	Ok(())
}

fn status_text(status: ModelStatus) -> &'static str {
	match status {
		ModelStatus::Starting => "starting",
		ModelStatus::Running => "running",
		ModelStatus::Stopped => "stopped",
		ModelStatus::Failed => "failed",
		ModelStatus::Removing => "being removed",
	}
}

/// # Errors
/// `Forbidden` without `read model`, `NotFound` for another team's project.
pub async fn authorize_read(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Read, Resource::Model).await?;
	scope::project(&s.db, team, project).await?;
	Ok(())
}

/// # Errors
/// `Forbidden` without `read model`, `NotFound` for another team's project.
pub async fn list(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<Vec<Model>> {
	authorize(&s.db, user, team, Action::Read, Resource::Model).await?;
	scope::project(&s.db, team, project).await?;
	Ok(models::list_by_project(&s.db, project).await?)
}

/// # Errors
/// `Forbidden` without `read model`, `NotFound` for another team's model.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
) -> AppResult<Model> {
	authorize(&s.db, user, team, Action::Read, Resource::Model).await?;
	scope::model(&s.db, team, project, id).await
}

/// Redeploys a stopped or failed model and waits for it to load again.
///
/// # Errors
/// `Conflict` unless the model is stopped or failed.
pub async fn start(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
) -> AppResult<Model> {
	authorize(&s.db, user, team, Action::Update, Resource::Model).await?;
	let mut model = scope::model(&s.db, team, project, id).await?;
	if !matches!(model.status, ModelStatus::Stopped | ModelStatus::Failed) {
		return Err(ServerError::Conflict(format!(
			"the model is {}; only a stopped or failed model can be started",
			status_text(model.status)
		)));
	}
	begin_start(s, &mut model).await?;
	Ok(model)
}

/// Stops the container; the agent will not restart it.
///
/// # Errors
/// `Conflict` when there is nothing to stop.
pub async fn stop(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
) -> AppResult<Model> {
	authorize(&s.db, user, team, Action::Update, Resource::Model).await?;
	let model = scope::model(&s.db, team, project, id).await?;
	let stoppable = match model.status {
		ModelStatus::Running | ModelStatus::Starting => true,
		ModelStatus::Failed => model.container_id.is_some(),
		ModelStatus::Stopped | ModelStatus::Removing => false,
	};
	if !stoppable {
		return Err(ServerError::Conflict(format!(
			"the model is {}; there is nothing to stop",
			status_text(model.status)
		)));
	}
	queue(s, JobKind::ModelStop, &model).await?;
	Ok(model)
}

/// Recreates the container from the stored settings.
///
/// # Errors
/// `Conflict` while the model is being removed.
pub async fn restart(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
) -> AppResult<Model> {
	authorize(&s.db, user, team, Action::Update, Resource::Model).await?;
	let mut model = scope::model(&s.db, team, project, id).await?;
	if model.status == ModelStatus::Removing {
		return Err(ServerError::Conflict(
			"the model is being removed".to_owned(),
		));
	}
	begin_start(s, &mut model).await?;
	Ok(model)
}

async fn begin_start(s: &AppState, model: &mut Model) -> AppResult<()> {
	models::set_status(&s.db, model.id, ModelStatus::Starting, None).await?;
	model.status = ModelStatus::Starting;
	model.error = None;
	queue(s, JobKind::ModelStart, model).await
}

/// Marks the model removing and queues the removal of its container; the row
/// goes when the job finishes. The downloaded files stay.
///
/// # Errors
/// `Conflict` when a removal is already under way.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
) -> AppResult<Model> {
	authorize(&s.db, user, team, Action::Delete, Resource::Model).await?;
	let mut model = scope::model(&s.db, team, project, id).await?;
	if model.status == ModelStatus::Removing {
		return Err(ServerError::Conflict(
			"the model is already being removed".to_owned(),
		));
	}
	models::set_status(&s.db, model.id, ModelStatus::Removing, None).await?;
	model.status = ModelStatus::Removing;
	model.error = None;
	queue(s, JobKind::ModelRemove, &model).await?;
	Ok(model)
}

/// The container's output, `tail` lines (at most 5000) and then new lines when `follow`.
///
/// # Errors
/// `Validation` before the first start has made a container.
pub async fn logs(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
	tail: u32,
	follow: bool,
) -> AppResult<BoxStream<'static, Result<Event, ServerError>>> {
	authorize(&s.db, user, team, Action::Read, Resource::Model).await?;
	let model = scope::model(&s.db, team, project, id).await?;
	let Some(container_id) = model.container_id.as_deref() else {
		return Err(ServerError::Validation(
			"the model has no container yet".to_owned(),
		));
	};
	let server = scope::server(&s.db, team, model.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let path = format!(
		"/v1/containers/{container_id}/logs?tail={}&follow={follow}",
		tail.min(MAX_TAIL)
	);
	client.stream(Method::GET, &path, None::<&()>).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slugs_are_dns_safe() {
		assert_eq!(slugify("Qwen3 4B (Q4)"), "qwen3-4b-q4");
		assert_eq!(slugify("  --x--  "), "x");
		assert_eq!(slugify("***"), "model");
		assert_eq!(slugify(&"a".repeat(80)).len(), 40);
		assert_eq!(slugify("Ünïcode"), "n-code");
	}

	#[test]
	fn estimates_scale_with_weights_and_context() {
		// Qwen3-4B Q4_K_M, 8k context.
		let e = estimate(2_497_280_256, 8192);
		assert_eq!(e.weights, 2382);
		assert_eq!(e.context, 1024);
		assert_eq!(e.overhead, 512 + 357);
		assert_eq!(e.total_mb(), 2382 + 1024 + 869);
		assert!(estimate(2_497_280_256, 32_768).total_mb() > e.total_mb());
		assert_eq!(estimate(0, 0).total_mb(), 512);
	}
}
