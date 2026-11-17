use std::collections::HashMap;
use std::collections::hash_map::Entry;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use runtime::{ContainerId, ContainerRuntime, ServerConnection};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{
	Application, ApplicationUpdate, BuildStrategy, NewApplication, SLUG_CONSTRAINT, public_hostname,
};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::deployments::{self, DeploymentResponse, DeploymentStatus};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId};
use crate::servers::Server;
use crate::state::AppState;
use crate::{db, deploy_pipeline, scope, validate};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{application_id}", delete(remove).patch(update))
		.route("/{application_id}/stop", post(stop))
		.route("/{application_id}/restart", post(restart))
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

impl ApplicationResponse {
	/// `url` is derived from the server the app runs on, so the response
	/// needs it alongside the row.
	fn new(app: &Application, server: &Server) -> Self {
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
			url: public_hostname(&app.slug, server),
			webhook_secret: None,
		}
	}
}

#[derive(Serialize)]
pub struct DeleteApplicationResponse {
	pub id: ApplicationId,
}

/// Checks the repo URL, Dockerfile path, and that each credential matches
/// the URL scheme: deploy keys are SSH-only, tokens are HTTPS-only.
fn validate_source(
	repo_url: &str,
	dockerfile_path: &str,
	deploy_key: Option<&str>,
	git_token: Option<&str>,
) -> AppResult<()> {
	if !validate::repo_url(repo_url) {
		return Err(ServerError::Validation("invalid repo_url".to_string()));
	}
	if !validate::rel_path(dockerfile_path) {
		return Err(ServerError::Validation(
			"invalid dockerfile_path".to_string(),
		));
	}
	let https = repo_url.starts_with("https://");
	if deploy_key.is_some_and(|k| !k.is_empty()) && https {
		return Err(ServerError::Validation(
			"deploy_key requires an ssh:// or scp-form repo_url, use git_token for https://"
				.to_string(),
		));
	}
	if git_token.is_some_and(|t| !t.is_empty()) && !https {
		return Err(ServerError::Validation(
			"git_token requires an https:// repo_url, use deploy_key for ssh".to_string(),
		));
	}
	Ok(())
}

/// Encrypts a write-only credential; `None` and empty both mean "not provided".
fn seal(state: &AppState, value: Option<&str>, what: &str) -> AppResult<Option<Vec<u8>>> {
	value
		.filter(|v| !v.is_empty())
		.map(|v| state.secrets.encrypt(v))
		.transpose()
		.map_err(|e| ServerError::Validation(format!("failed to encrypt {what}: {e}")))
}

#[allow(clippy::cast_possible_truncation)]
fn cpu_to_f32(cpu: Option<f64>) -> Option<f32> {
	cpu.map(|c| c as f32)
}

fn slug_conflict(e: sqlx::Error) -> ServerError {
	if db::is_unique_violation(&e, SLUG_CONSTRAINT) {
		ServerError::ApplicationSlugTaken
	} else {
		e.into()
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
	authorize(&s.db, user.id, team, Action::Create, Resource::Application).await?;
	scope::project(&s.db, team, project).await?;
	let server = scope::server(&s.db, team, body.server_id).await?;

	validate_source(
		&body.repo_url,
		&body.dockerfile_path,
		body.deploy_key.as_deref(),
		body.git_token.as_deref(),
	)?;
	let deploy_key = seal(&s, body.deploy_key.as_deref(), "deploy key")?;
	let git_token = seal(&s, body.git_token.as_deref(), "git token")?;

	let webhook_secret_plain = crypto::generate_token(24);
	let webhook_secret = s
		.secrets
		.encrypt(&webhook_secret_plain)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt webhook secret: {e}")))?;

	let app = super::insert(
		&s.db,
		NewApplication {
			id: ApplicationId::generate(),
			project_id: project,
			server_id: server.id,
			name: body.name,
			slug: body.slug,
			repo_url: body.repo_url,
			git_ref: body.git_ref,
			deploy_key,
			git_token,
			build_strategy: BuildStrategy::parse(&body.build_strategy),
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			webhook_secret: Some(webhook_secret),
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: cpu_to_f32(body.cpu_limit),
		},
	)
	.await
	.map_err(slug_conflict)?;

	let mut response = ApplicationResponse::new(&app, &server);
	response.webhook_secret = Some(webhook_secret_plain);
	Ok(Json(response))
}

/// Lists applications under a project; caller must hold `read application`
/// on the team. The project must belong to that team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
) -> AppResult<Json<Vec<ApplicationResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Application).await?;
	scope::project(&s.db, team, project).await?;

	let apps = super::list_by_project(&s.db, project).await?;
	let mut servers: HashMap<ServerId, Server> = HashMap::new();
	let mut out = Vec::with_capacity(apps.len());
	for app in &apps {
		if let Entry::Vacant(slot) = servers.entry(app.server_id) {
			slot.insert(scope::server(&s.db, team, app.server_id).await?);
		}
		out.push(ApplicationResponse::new(app, &servers[&app.server_id]));
	}
	Ok(Json(out))
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
	authorize(&s.db, user.id, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, id).await?;
	let server = scope::server(&s.db, team, app.server_id).await?;

	validate_source(
		&body.repo_url,
		&body.dockerfile_path,
		body.deploy_key.as_deref(),
		body.git_token.as_deref(),
	)?;
	let deploy_key = seal(&s, body.deploy_key.as_deref(), "deploy key")?;
	let git_token = seal(&s, body.git_token.as_deref(), "git token")?;

	let updated = super::update(
		&s.db,
		app.id,
		ApplicationUpdate {
			name: body.name,
			slug: body.slug,
			repo_url: body.repo_url,
			git_ref: body.git_ref,
			deploy_key,
			git_token,
			build_strategy: BuildStrategy::parse(&body.build_strategy),
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: cpu_to_f32(body.cpu_limit),
		},
	)
	.await
	.map_err(slug_conflict)?;

	Ok(Json(ApplicationResponse::new(&updated, &server)))
}

/// Deletes an application; caller must hold `delete application` on the
/// team. The application must belong to the project named in the path.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<DeleteApplicationResponse>> {
	authorize(&s.db, user.id, team, Action::Delete, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, id).await?;
	super::delete(&s.db, app.id).await?;
	Ok(Json(DeleteApplicationResponse { id: app.id }))
}

/// Stops the application's currently running deployment: a manual halt,
/// distinct from the automatic `Running -> Stopped` that happens when a
/// redeploy swaps it out (that's the one other place a running deployment
/// gets stopped this way). To bring the app back use
/// `POST .../applications/{aid}/restart`, which redeploys the same image.
///
/// The DB transition is persisted *before* the container is actually
/// stopped, not after: `health_monitor` only "heals" (restarts) deployments
/// still marked `Running`, so flipping the status first is what keeps it
/// from racing in and undoing this on its next 15s sweep. Once persisted,
/// the container stop/remove is best-effort, matching how
/// `deploy_pipeline::retire_previous_container` already treats infra
/// cleanup as secondary to the authoritative DB state.
///
/// Caller must hold `update deployment` on the team.
pub async fn stop(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<DeploymentResponse>> {
	authorize(&s.db, user.id, team, Action::Update, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, id).await?;

	let current = deployments::latest_running(&s.db, app.id)
		.await?
		.ok_or_else(|| {
			ServerError::Validation("application is not currently running".to_string())
		})?;
	let Some(container_id) = current.container_id.clone() else {
		return Err(ServerError::Validation(
			"running deployment has no container recorded".to_string(),
		));
	};

	let stopped =
		deployments::set_status(&s.db, current.id, DeploymentStatus::Stopped, None).await?;

	if let Ok(server) = scope::server(&s.db, team, app.server_id).await {
		match ServerConnection::connect(&server.connect_target(), &s.secrets).await {
			Ok(connection) => {
				let cid = ContainerId(container_id);
				if let Err(e) = connection.runtime.stop_container(&cid).await {
					warn!(deployment = stopped.id.0, "failed to stop container: {e}");
				} else if let Err(e) = connection.runtime.remove_container(&cid, true).await {
					warn!(
						deployment = stopped.id.0,
						"failed to remove stopped container: {e}"
					);
				}
			}
			Err(e) => warn!(
				deployment = stopped.id.0,
				"failed to connect to server to stop container: {e}"
			),
		}
	} else {
		warn!(
			deployment = stopped.id.0,
			"target server no longer exists, deployment marked stopped, container left as-is"
		);
	}

	Ok(Json(DeploymentResponse::from(&stopped)))
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
	authorize(&s.db, user.id, team, Action::Create, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, id).await?;

	// Newest build with an image, regardless of whether it's currently
	// Stopped, Failed (a later attempt failed but an earlier one still has a
	// usable image), or even still Running (restarting a healthy app is a
	// valid "just in case").
	let target = deployments::latest_with_image(&s.db, app.id)
		.await?
		.ok_or_else(|| {
			ServerError::Validation(
				"no previous successful build to restart from, trigger a deploy instead"
					.to_string(),
			)
		})?;

	let deployment =
		deploy_pipeline::queue_rollback_and_spawn(&s, app, &target, Some(user.id)).await?;

	Ok((
		StatusCode::ACCEPTED,
		Json(DeploymentResponse::from(&deployment)),
	))
}
