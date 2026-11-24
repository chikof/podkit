//! HTTP handlers of the models slice. They only translate: the rules live in
//! [`super::service`].

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::engines::{self, EngineKind, Format, GpuNeed};
use super::service::{self, CreateModel};
use super::{Model, ModelStatus};
use crate::applications::routes::log_events;
use crate::auth::AuthUser;
use crate::environments::EnvironmentFilter;
use crate::error::AppResult;
use crate::ids::{EnvironmentId, ModelFileId, ModelId, ProjectId, ServerId, TeamId};
use crate::state::AppState;
use proto::GpuVendor;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/engines", get(engines_catalog))
		.route("/{model_id}", get(show).delete(remove))
		.route("/{model_id}/start", post(start))
		.route("/{model_id}/stop", post(stop))
		.route("/{model_id}/restart", post(restart))
		.route("/{model_id}/logs", get(logs))
}

#[derive(Deserialize)]
struct CreateRequest {
	name: String,
	/// The downloaded files to serve; the model runs on their server.
	model_files_id: ModelFileId,
	engine: EngineKind,
	/// The `model` clients send; defaults to the model's slug.
	served_name: Option<String>,
	/// File or directory inside the download, like `Qwen3-4B-Q4_K_M.gguf`.
	model_path: Option<String>,
	/// Tokens; 8192 when absent.
	context_size: Option<u32>,
	/// Run on a GPU. The engine decides when absent.
	gpu: Option<bool>,
	/// Extra engine arguments, one array element each; no shell is involved.
	#[serde(default)]
	extra_args: Vec<String>,
	/// Only for the `custom` engine.
	image: Option<String>,
	/// Only for the `custom` engine; 8000 when absent.
	port: Option<u16>,
	memory_limit_mb: Option<i32>,
	/// The project's first environment when omitted.
	#[serde(default)]
	environment_id: Option<EnvironmentId>,
}

#[derive(Serialize)]
struct ModelResponse {
	id: ModelId,
	project_id: ProjectId,
	environment_id: EnvironmentId,
	server_id: ServerId,
	model_files_id: ModelFileId,
	name: String,
	slug: String,
	engine: EngineKind,
	image: String,
	port: i32,
	model_path: String,
	served_name: String,
	context_size: i32,
	gpu: bool,
	extra_args: Vec<String>,
	memory_limit_mb: Option<i32>,
	status: ModelStatus,
	error: Option<String>,
	container_id: Option<String>,
	/// Where other containers on the server reach the OpenAI-compatible API.
	/// Never published on the host.
	internal_url: String,
	created_at: String,
	updated_at: String,
}

impl From<Model> for ModelResponse {
	fn from(m: Model) -> Self {
		Self {
			internal_url: m.internal_url(),
			id: m.id,
			project_id: m.project_id,
			environment_id: m.environment_id,
			server_id: m.server_id,
			model_files_id: m.model_files_id,
			name: m.name,
			slug: m.slug,
			engine: m.engine,
			image: m.image,
			port: m.port,
			model_path: m.model_path,
			served_name: m.served_name,
			context_size: m.context_size,
			gpu: m.gpu,
			extra_args: m.extra_args.0,
			memory_limit_mb: m.memory_limit_mb,
			status: m.status,
			error: m.error,
			container_id: m.container_id,
			created_at: m.created_at.to_string(),
			updated_at: m.updated_at.to_string(),
		}
	}
}

type Accepted = AppResult<(StatusCode, Json<ModelResponse>)>;

#[derive(Serialize)]
struct EngineInfo {
	engine: EngineKind,
	label: &'static str,
	formats: Vec<Format>,
	/// `never`, `optional` or `required`.
	gpu: &'static str,
	gpu_vendor: Option<GpuVendor>,
	port: u16,
	/// The pinned image; absent for `custom`, where the caller names it.
	image: Option<String>,
	context_flag: &'static str,
	gpu_layers_flag: Option<&'static str>,
	accepts_extra_args: bool,
	stop_grace_s: u32,
}

/// The engines a model can run on and what each accepts. Caller must hold
/// `read model`.
async fn engines_catalog(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<Vec<EngineInfo>>> {
	service::authorize_read(&s, user.id, team, project).await?;
	Ok(Json(
		engines::all()
			.iter()
			.map(|e| EngineInfo {
				engine: e.kind,
				label: e.label,
				formats: e.formats.to_vec(),
				gpu: match e.gpu {
					GpuNeed::Never => "never",
					GpuNeed::Optional => "optional",
					GpuNeed::Required => "required",
				},
				gpu_vendor: e.gpu_vendor,
				port: e.port,
				image: (e.kind != EngineKind::Custom).then(|| e.image(false, None)),
				context_flag: e.context_flag,
				gpu_layers_flag: e.gpu_layers_flag,
				accepts_extra_args: e.takes_extra_args(),
				stop_grace_s: e.stop_grace_s,
			})
			.collect(),
	))
}

fn accepted(model: Model) -> (StatusCode, Json<ModelResponse>) {
	(StatusCode::ACCEPTED, Json(model.into()))
}

/// Creates a model and queues its first start. Poll `GET` until `status` leaves
/// `starting`, or follow the job log. Caller must hold `create model`.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<CreateRequest>,
) -> Accepted {
	let model = service::create(
		&s,
		user.id,
		team,
		project,
		CreateModel {
			name: body.name,
			model_files_id: body.model_files_id,
			engine: body.engine,
			served_name: body.served_name,
			model_path: body.model_path,
			context_size: body.context_size,
			gpu: body.gpu,
			extra_args: body.extra_args,
			image: body.image,
			port: body.port,
			memory_limit_mb: body.memory_limit_mb,
			environment_id: body.environment_id,
		},
	)
	.await?;
	Ok(accepted(model))
}

/// Lists a project's models. Caller must hold `read model`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(filter): Query<EnvironmentFilter>,
) -> AppResult<Json<Vec<ModelResponse>>> {
	let rows = service::list(&s, user.id, team, project, filter.environment_id).await?;
	Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// Caller must hold `read model`.
async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
) -> AppResult<Json<ModelResponse>> {
	Ok(Json(
		service::get(&s, user.id, team, project, id).await?.into(),
	))
}

/// Redeploys a stopped or failed model. Caller must hold `update model`.
async fn start(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
) -> Accepted {
	Ok(accepted(
		service::start(&s, user.id, team, project, id).await?,
	))
}

/// Stops the container. Caller must hold `update model`.
async fn stop(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
) -> Accepted {
	Ok(accepted(
		service::stop(&s, user.id, team, project, id).await?,
	))
}

/// Recreates the container. Caller must hold `update model`.
async fn restart(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
) -> Accepted {
	Ok(accepted(
		service::restart(&s, user.id, team, project, id).await?,
	))
}

/// Removes the container and the model; the downloaded files stay. Caller must
/// hold `delete model`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
) -> Accepted {
	Ok(accepted(
		service::remove(&s, user.id, team, project, id).await?,
	))
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

/// Streams the container's stdout/stderr as server-sent events
/// (`event: log`, data `{"stream","line"}`, then `event: end`), like
/// application runtime logs. `?tail=` is capped at 5000 lines. Caller must
/// hold `read model`.
async fn logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
	Query(params): Query<LogsParams>,
) -> AppResult<impl IntoResponse> {
	let events = service::logs(&s, user.id, team, project, id, params.tail, params.follow).await?;
	Ok(Sse::new(log_events(events, s.shutdown.clone())).keep_alive(KeepAlive::default()))
}
