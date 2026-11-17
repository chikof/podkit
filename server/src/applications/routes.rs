use std::convert::Infallible;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use futures_util::stream::{BoxStream, Stream};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::service::{self, AppInput, ApplicationView};
use crate::auth::AuthUser;
use crate::deployments::{self, DeploymentResponse};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{application_id}", delete(remove).patch(update))
		.route("/{application_id}/stop", post(stop))
		.route("/{application_id}/restart", post(restart))
		.route("/{application_id}/logs", get(logs))
}

fn default_git_ref() -> String {
	"main".to_string()
}

fn default_dockerfile_path() -> String {
	"Dockerfile".to_string()
}

fn default_build_strategy() -> String {
	"dockerfile".to_string()
}

#[derive(Deserialize)]
pub struct CreateApplicationRequest {
	pub name: String,
	pub slug: String,
	pub server_id: ServerId,
	pub repo_url: String,
	#[serde(default = "default_git_ref")]
	pub git_ref: String,
	/// SSH private key (PEM) for `ssh://`/scp-form repo URLs. Plaintext on the
	/// wire (TLS-protected); encrypted at rest. `None`/empty for public repos.
	#[serde(default)]
	pub deploy_key: Option<String>,
	/// Access token for `https://` repo URLs. Plaintext on the wire
	/// (TLS-protected); encrypted at rest. `None`/empty for public repos.
	#[serde(default)]
	pub git_token: Option<String>,
	#[serde(default = "default_build_strategy")]
	pub build_strategy: String,
	#[serde(default = "default_dockerfile_path")]
	pub dockerfile_path: String,
	pub container_port: i32,
	#[serde(default)]
	pub memory_limit_mb: Option<i32>,
	#[serde(default)]
	pub cpu_limit: Option<f64>,
}

/// Same shape as `CreateApplicationRequest` minus `server_id` (moving an
/// app to a different server isn't supported, that's a re-provisioning
/// operation, not a config edit). This lets a settings form reuse the
/// create form's fields verbatim, pre-filled from `GET .../applications`.
#[derive(Deserialize)]
pub struct UpdateApplicationRequest {
	pub name: String,
	pub slug: String,
	pub repo_url: String,
	#[serde(default = "default_git_ref")]
	pub git_ref: String,
	/// Omitted or empty leaves the stored deploy key untouched (it's
	/// write-only, never echoed back, so a settings form can't prefill
	/// it). A non-empty value replaces it. There is currently no way to
	/// explicitly clear a deploy key back to "public repo" once set.
	#[serde(default)]
	pub deploy_key: Option<String>,
	/// Same write-only semantics as `deploy_key`, for `https://` repos.
	#[serde(default)]
	pub git_token: Option<String>,
	#[serde(default = "default_build_strategy")]
	pub build_strategy: String,
	#[serde(default = "default_dockerfile_path")]
	pub dockerfile_path: String,
	pub container_port: i32,
	#[serde(default)]
	pub memory_limit_mb: Option<i32>,
	#[serde(default)]
	pub cpu_limit: Option<f64>,
}

/// Never carries `deploy_key` or `git_token`.
#[derive(Serialize)]
pub struct ApplicationResponse {
	pub id: ApplicationId,
	pub project_id: ProjectId,
	pub server_id: ServerId,
	pub name: String,
	pub slug: String,
	pub repo_url: String,
	pub git_ref: String,
	pub build_strategy: String,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	pub url: String,
	/// Only ever populated on creation, the plaintext is not recoverable
	/// afterwards (only the encrypted form is stored). Wire this into the
	/// git provider's webhook config to enable `POST /webhooks/deploy/{id}`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub webhook_secret: Option<String>,
}

#[derive(Serialize)]
pub struct DeleteApplicationResponse {
	pub id: ApplicationId,
}

impl From<&ApplicationView> for ApplicationResponse {
	fn from(app: &ApplicationView) -> Self {
		Self {
			id: app.id,
			project_id: app.project_id,
			server_id: app.server_id,
			name: app.name.clone(),
			slug: app.slug.clone(),
			repo_url: app.repo_url.clone(),
			git_ref: app.git_ref.clone(),
			build_strategy: app.build_strategy.as_str().to_string(),
			dockerfile_path: app.dockerfile_path.clone(),
			container_port: app.container_port,
			memory_limit_mb: app.memory_limit_mb,
			cpu_limit: app.cpu_limit.map(f64::from),
			url: app.url.clone(),
			webhook_secret: None,
		}
	}
}

/// Creates an application under a project, deployed to one of the team's
/// own servers. Caller must hold `create application` on the team; both the
/// project and the target server must belong to that same team.
pub async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<CreateApplicationRequest>,
) -> AppResult<Json<ApplicationResponse>> {
	let created = service::create(
		&s,
		user.id,
		team,
		project,
		body.server_id,
		AppInput {
			name: body.name,
			slug: body.slug,
			repo_url: body.repo_url,
			git_ref: body.git_ref,
			deploy_key: body.deploy_key,
			git_token: body.git_token,
			build_strategy: body.build_strategy,
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: body.cpu_limit,
		},
	)
	.await?;
	let mut response = ApplicationResponse::from(&created.app);
	response.webhook_secret = Some(created.webhook_secret);
	Ok(Json(response))
}

/// Lists applications under a project; caller must hold `read application`
/// on the team. The project must belong to that team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<Vec<ApplicationResponse>>> {
	let apps = service::list(&s.db, user.id, team, project).await?;
	Ok(Json(apps.iter().map(ApplicationResponse::from).collect()))
}

/// Updates an application's configuration (everything set at creation
/// except `server_id`, which is immutable, see `UpdateApplicationRequest`
/// doc comment). Takes effect on the *next* deploy: an already-running
/// container isn't touched, same as env var changes. Caller must hold
/// `update application` on the team.
pub async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<UpdateApplicationRequest>,
) -> AppResult<Json<ApplicationResponse>> {
	let updated = service::update(
		&s,
		user.id,
		team,
		project,
		id,
		AppInput {
			name: body.name,
			slug: body.slug,
			repo_url: body.repo_url,
			git_ref: body.git_ref,
			deploy_key: body.deploy_key,
			git_token: body.git_token,
			build_strategy: body.build_strategy,
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: body.cpu_limit,
		},
	)
	.await?;
	Ok(Json(ApplicationResponse::from(&updated)))
}

/// Deletes an application; caller must hold `delete application` on the
/// team. The application must belong to the project named in the path.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<DeleteApplicationResponse>> {
	let id = service::remove(&s, user.id, team, project, id).await?;
	Ok(Json(DeleteApplicationResponse { id }))
}

/// Stops the application's currently running deployment: a manual halt,
/// distinct from the automatic `Running -> Stopped` that happens when a
/// redeploy swaps it out. To bring the app back use
/// `POST .../applications/{aid}/restart`, which redeploys the same image.
///
/// The agent stops the container and takes it off its restart list before
/// the row moves to `stopped`, so a failed stop leaves the deployment
/// `running` and the call can be retried.
///
/// Caller must hold `update deployment` on the team.
pub async fn stop(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<DeploymentResponse>> {
	let stopped = deployments::service::stop(&s, user.id, team, project, id).await?;
	Ok(Json(DeploymentResponse::from(&stopped)))
}

#[derive(Deserialize)]
pub struct LogsParams {
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

/// Streams the running container's stdout/stderr as server-sent events
/// (`event: log`, data `{"stream","line"}`), proxied from the server agent
/// until the container exits or the client disconnects (`event: end`).
/// `?tail=` is capped at 5000 lines. Caller must hold `read application`.
pub async fn logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(params): Query<LogsParams>,
) -> AppResult<impl IntoResponse> {
	let events = deployments::service::runtime_log(
		&s,
		user.id,
		team,
		project,
		id,
		params.tail,
		params.follow,
	)
	.await?;

	Ok(Sse::new(log_events(events, s.shutdown.clone())).keep_alive(KeepAlive::default()))
}

pub fn log_events(
	events: BoxStream<'static, Result<proto::Event, ServerError>>,
	shutdown: tokio_util::sync::CancellationToken,
) -> impl Stream<Item = Result<SseEvent, Infallible>> {
	futures_util::stream::unfold(Some(events), move |events| {
		let shutdown = shutdown.clone();
		async move {
			let mut events = events?;
			loop {
				let next = tokio::select! {
					biased;
					() = shutdown.cancelled() => None,
					next = events.next() => next,
				};
				match agent_log_step(next) {
					AgentLogStep::Line { stream, line } => {
						let data =
							serde_json::json!({ "stream": stream, "line": line }).to_string();
						return Some((
							Ok(SseEvent::default().event("log").data(data)),
							Some(events),
						));
					}
					AgentLogStep::Skip => {}
					AgentLogStep::End { .. } => break,
				}
			}
			Some((Ok(SseEvent::default().event("end")), None))
		}
	})
}

/// What one item of an agent log stream means to a log viewer.
pub(crate) enum AgentLogStep {
	Line {
		stream: proto::Stream,
		line: String,
	},
	/// The stream is over; `failed` says whether it broke rather than finished.
	End {
		failed: bool,
	},
	/// An event that carries no log output.
	Skip,
}

/// Maps one agent event to a [`AgentLogStep`], logging failures. Shared by the
/// JSON/SSE endpoint and the web UI so both read the stream the same way.
pub(crate) fn agent_log_step(next: Option<Result<proto::Event, ServerError>>) -> AgentLogStep {
	match next {
		Some(Ok(proto::Event::Log { stream, line })) => AgentLogStep::Line { stream, line },
		Some(Ok(proto::Event::Failed { message })) => {
			warn!("container log stream failed: {message}");
			AgentLogStep::End { failed: true }
		}
		Some(Err(e)) => {
			warn!("container log stream failed: {e}");
			AgentLogStep::End { failed: true }
		}
		Some(Ok(proto::Event::Done)) | None => AgentLogStep::End { failed: false },
		Some(Ok(_)) => AgentLogStep::Skip,
	}
}

/// Brings a stopped (or crash-looped-out) application back up by
/// redeploying its most recent successfully-built image. No git clone or
/// rebuild involved, identical mechanism to
/// `POST .../deployments/{did}/rollback`, just aimed at "whichever image
/// was live most recently" instead of requiring the caller to know a
/// specific deployment id. Caller must hold `create deployment` on the
/// team, same permission rollback uses since this is a deploy action, not
/// a distinct one.
///
/// # Errors
/// Returns [`ServerError::Validation`] if the application has never
/// produced a build to redeploy. In that case there's nothing to restart,
/// the caller should trigger a full deploy instead.
pub async fn restart(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	let deployment = deployments::service::restart(&s, user.id, team, project, id).await?;

	Ok((
		StatusCode::ACCEPTED,
		Json(DeploymentResponse::from(&deployment)),
	))
}
