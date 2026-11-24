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
use crate::change::Change;
use crate::deployments::{self, DeploymentResponse};
use crate::environments::EnvironmentFilter;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, DeploymentId, EnvironmentId, ProjectId, RegistryId, ServerId, TeamId,
};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{application_id}", delete(remove).patch(update))
		.route("/{application_id}/stop", post(stop))
		.route("/{application_id}/restart", post(restart))
		.route(
			"/{application_id}/promote",
			get(promotion_plan).post(promote),
		)
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
	/// Git source; mutually exclusive with `image`.
	#[serde(default)]
	pub repo_url: String,
	/// Registry image source (`traefik/whoami:v1`); mutually exclusive with `repo_url`.
	#[serde(default)]
	pub image: Option<String>,
	/// Write-only registry credentials for a private `image`.
	#[serde(default)]
	pub registry_username: Option<String>,
	#[serde(default)]
	pub registry_password: Option<String>,
	/// A saved registry (`/teams/{team}/registries`) to pull with. Omitted keeps the current
	/// one on update; `null` clears it.
	#[serde(default)]
	pub registry_id: Change<RegistryId>,
	#[serde(default = "default_git_ref")]
	pub git_ref: String,
	/// SSH private key to import into the vault as this application's deploy key, for
	/// `ssh://`/scp-form repo URLs (prefer `deploy_key_id`). Plaintext on the
	/// wire (TLS-protected); encrypted at rest. `None`/empty for public repos.
	#[serde(default)]
	pub deploy_key: Option<String>,
	/// A key from the team's SSH key vault to use as the deploy key.
	#[serde(default)]
	pub deploy_key_id: Option<crate::ids::SshKeyId>,
	/// Make a new Ed25519 deploy key; read its public key from the ssh-keys list.
	#[serde(default)]
	pub generate_deploy_key: bool,
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
	/// The project's first environment when omitted.
	#[serde(default)]
	pub environment_id: Option<EnvironmentId>,
	/// Clone `git_repo` through this git provider instead of from a `repo_url`.
	#[serde(default)]
	pub git_provider_id: Option<crate::ids::GitProviderId>,
	/// `owner/name` at the provider.
	#[serde(default)]
	pub git_repo: String,
}

/// Same shape as `CreateApplicationRequest` minus `server_id` (moving an
/// app to a different server isn't supported, that's a re-provisioning
/// operation, not a config edit). This lets a settings form reuse the
/// create form's fields verbatim, pre-filled from `GET .../applications`.
#[derive(Deserialize)]
pub struct UpdateApplicationRequest {
	pub name: String,
	pub slug: String,
	#[serde(default)]
	pub repo_url: String,
	#[serde(default)]
	pub image: Option<String>,
	/// Omitted or empty keeps the stored registry credentials.
	#[serde(default)]
	pub registry_username: Option<String>,
	#[serde(default)]
	pub registry_password: Option<String>,
	/// A saved registry (`/teams/{team}/registries`) to pull with. Omitted keeps the current
	/// one on update; `null` clears it.
	#[serde(default)]
	pub registry_id: Change<RegistryId>,
	#[serde(default = "default_git_ref")]
	pub git_ref: String,
	/// Omitted or empty leaves the stored deploy key untouched (it's
	/// write-only, never echoed back, so a settings form can't prefill
	/// it). A non-empty value replaces it. There is currently no way to
	/// explicitly clear a deploy key back to "public repo" once set.
	#[serde(default)]
	pub deploy_key: Option<String>,
	/// A key from the team's SSH key vault to use as the deploy key.
	#[serde(default)]
	pub deploy_key_id: Option<crate::ids::SshKeyId>,
	/// Make a new Ed25519 deploy key; read its public key from the ssh-keys list.
	#[serde(default)]
	pub generate_deploy_key: bool,
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

/// Never carries `deploy_key`, `git_token` or registry credentials.
#[derive(Serialize)]
pub struct ApplicationResponse {
	pub id: ApplicationId,
	pub project_id: ProjectId,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub name: String,
	pub slug: String,
	/// `git` or `image`.
	pub source_kind: String,
	pub repo_url: Option<String>,
	pub image: Option<String>,
	/// Whether registry credentials are stored; they never leave the server.
	pub has_registry_auth: bool,
	/// The saved registry the image is pulled with.
	pub registry_id: Option<RegistryId>,
	pub git_ref: String,
	pub build_strategy: String,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	pub url: String,
	/// The vault key used as the deploy key (its public half is in the ssh-keys list).
	pub deploy_key_id: Option<crate::ids::SshKeyId>,
	/// The git provider the repository is cloned through, and the repository there.
	pub git_provider_id: Option<crate::ids::GitProviderId>,
	pub git_repo: Option<String>,
	/// Set on creation when the push webhook could not be put in place.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub notice: Option<String>,
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
			environment_id: app.environment_id,
			server_id: app.server_id,
			name: app.name.clone(),
			slug: app.slug.clone(),
			source_kind: app.source_kind.as_str().to_string(),
			repo_url: app.repo_url.clone(),
			image: app.image.clone(),
			has_registry_auth: app.has_registry_auth,
			registry_id: app.registry_id,
			git_ref: app.git_ref.clone(),
			build_strategy: app.build_strategy.as_str().to_string(),
			dockerfile_path: app.dockerfile_path.clone(),
			container_port: app.container_port,
			memory_limit_mb: app.memory_limit_mb,
			cpu_limit: app.cpu_limit.map(f64::from),
			url: app.url.clone(),
			deploy_key_id: app.deploy_key_id,
			git_provider_id: app.git_provider_id,
			git_repo: app.git_repo.clone(),
			notice: None,
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
			image: body.image,
			registry_username: body.registry_username,
			registry_password: body.registry_password,
			registry: body.registry_id,
			git_ref: body.git_ref,
			deploy_key: body.deploy_key,
			deploy_key_id: body.deploy_key_id,
			generate_deploy_key: body.generate_deploy_key,
			git_token: body.git_token,
			build_strategy: body.build_strategy,
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: body.cpu_limit,
			environment_id: body.environment_id,
			git_provider_id: body.git_provider_id,
			git_repo: body.git_repo,
		},
	)
	.await?;
	let mut response = ApplicationResponse::from(&created.app);
	response.webhook_secret = Some(created.webhook_secret);
	response.notice = created.notice;
	Ok(Json(response))
}

/// Lists applications under a project; caller must hold `read application`
/// on the team. The project must belong to that team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(filter): Query<EnvironmentFilter>,
) -> AppResult<Json<Vec<ApplicationResponse>>> {
	let apps = service::list(&s.db, user.id, team, project, filter.environment_id).await?;
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
			image: body.image,
			registry_username: body.registry_username,
			registry_password: body.registry_password,
			registry: body.registry_id,
			git_ref: body.git_ref,
			deploy_key: body.deploy_key,
			deploy_key_id: body.deploy_key_id,
			generate_deploy_key: body.generate_deploy_key,
			git_token: body.git_token,
			build_strategy: body.build_strategy,
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: body.cpu_limit,
			environment_id: None,
			git_provider_id: None,
			git_repo: String::new(),
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

#[derive(Serialize)]
pub struct EnvironmentRef {
	pub id: EnvironmentId,
	pub name: String,
	pub slug: String,
}

impl From<&crate::environments::Environment> for EnvironmentRef {
	fn from(e: &crate::environments::Environment) -> Self {
		Self {
			id: e.id,
			name: e.name.clone(),
			slug: e.slug.clone(),
		}
	}
}

/// The image a promotion would deploy.
#[derive(Serialize)]
pub struct PromotionSource {
	pub deployment_id: DeploymentId,
	pub commit_sha: Option<String>,
	pub image: Option<String>,
}

/// The application of another environment a promotion would deploy.
#[derive(Serialize)]
pub struct PromotionTarget {
	pub id: ApplicationId,
	pub name: String,
	pub slug: String,
	pub server_id: ServerId,
	pub server_name: String,
	/// What it runs now.
	pub running_deployment_id: Option<DeploymentId>,
	pub running_image: Option<String>,
}

#[derive(Serialize)]
pub struct PromotionDestination {
	pub environment: EnvironmentRef,
	/// `None` when nothing matches; `refusal` says why.
	pub application: Option<PromotionTarget>,
	pub refusal: Option<String>,
	/// The target runs on another server: the image is copied there before it starts.
	pub moves_image: bool,
	/// The target already runs this very image.
	pub already_running: bool,
}

#[derive(Serialize)]
pub struct PromotionPlanResponse {
	pub application_id: ApplicationId,
	pub environment: EnvironmentRef,
	/// `None` when the application runs nothing: there is no image to promote.
	pub source: Option<PromotionSource>,
	pub destinations: Vec<PromotionDestination>,
}

impl From<&deployments::promote::Plan> for PromotionPlanResponse {
	fn from(plan: &deployments::promote::Plan) -> Self {
		let image = plan.source.as_ref().and_then(|d| d.image_tag.as_deref());
		Self {
			application_id: plan.application.id,
			environment: (&plan.environment).into(),
			source: plan.source.as_ref().map(|d| PromotionSource {
				deployment_id: d.id,
				commit_sha: d.commit_sha.clone(),
				image: d.image_tag.clone(),
			}),
			destinations: plan
				.destinations
				.iter()
				.map(|d| match &d.matched {
					Ok(m) => PromotionDestination {
						environment: (&d.environment).into(),
						application: Some(PromotionTarget {
							id: m.application.id,
							name: m.application.name.clone(),
							slug: m.application.slug.clone(),
							server_id: m.server.id,
							server_name: m.server.name.clone(),
							running_deployment_id: m.running.as_ref().map(|r| r.id),
							running_image: m.running.as_ref().and_then(|r| r.image_tag.clone()),
						}),
						refusal: None,
						moves_image: m.moves_image,
						already_running: m.runs(image),
					},
					Err(why) => PromotionDestination {
						environment: (&d.environment).into(),
						application: None,
						refusal: Some(why.clone()),
						moves_image: false,
						already_running: false,
					},
				})
				.collect(),
		}
	}
}

/// What promoting the application would deploy, to each other environment of its project.
async fn promotion_plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<PromotionPlanResponse>> {
	let plan = deployments::promote::plan(&s.db, user.id, team, project, id).await?;
	Ok(Json(PromotionPlanResponse::from(&plan)))
}

#[derive(Deserialize)]
struct PromoteRequest {
	environment_id: EnvironmentId,
	/// The source deployment the caller reviewed; refused with 409 if it no longer runs.
	#[serde(default)]
	deployment_id: Option<DeploymentId>,
}

/// Deploys the image this application runs to the matching application of
/// `environment_id`, without rebuilding. Answers with the target's new deployment.
async fn promote(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<PromoteRequest>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	let promoted = deployments::promote::promote(
		&s,
		user.id,
		team,
		project,
		id,
		body.environment_id,
		body.deployment_id,
	)
	.await?;
	Ok((
		StatusCode::ACCEPTED,
		Json(DeploymentResponse::from(&promoted.deployment)),
	))
}
