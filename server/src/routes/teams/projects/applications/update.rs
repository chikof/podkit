use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::application::BuildStrategy;
use podkit_core::domain::shared::errors::DomainError;
use podkit_core::domain::shared::ids::{ApplicationId, ProjectId};
use serde::{Deserialize, Serialize};

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

fn default_git_ref() -> String {
	"main".to_string()
}

fn default_dockerfile_path() -> String {
	"Dockerfile".to_string()
}

fn default_build_strategy() -> String {
	"dockerfile".to_string()
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

#[derive(Serialize)]
pub struct ApplicationResponse {
	pub id: i64,
	pub project_id: i64,
	pub server_id: i64,
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
}

/// Updates an application's configuration (everything set at creation
/// except `server_id`, which is immutable, see `UpdateApplicationRequest`
/// doc comment). Takes effect on the *next* deploy: an already-running
/// container isn't touched, same as env var changes. Caller must hold
/// `update application` on the team.
pub async fn update_application(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id)): Path<(i64, i64, i64)>,
	Json(body): Json<UpdateApplicationRequest>,
) -> Result<Json<ApplicationResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"update",
		"application",
	)
	.await?;

	state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	let mut application = state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.filter(|a| a.project_id.0 == project_id)
		.ok_or(ServerError::ApplicationNotFound)?;

	let server = state
		.servers
		.find_by_id(application.server_id)
		.await?
		.ok_or(ServerError::ServerNotFound)?;

	application.name = body.name;
	application.slug = body.slug;
	application.repo_url = body.repo_url;
	application.git_ref = body.git_ref;
	application.build_strategy = BuildStrategy::parse(&body.build_strategy);
	application.dockerfile_path = body.dockerfile_path;
	application.container_port = body.container_port;
	application.memory_limit_mb = body.memory_limit_mb;
	application.cpu_limit = body.cpu_limit;

	if let Some(key) = body.deploy_key.filter(|k| !k.is_empty()) {
		let encrypted = state
			.secrets
			.encrypt(&key)
			.map_err(|e| ServerError::Validation(format!("failed to encrypt deploy key: {e}")))?;
		application.deploy_key = Some(encrypted);
	}

	state
		.applications
		.update(&application)
		.await
		.map_err(|e| match e {
			DomainError::AlreadyExists => ServerError::ApplicationSlugTaken,
			e => ServerError::Domain(e),
		})?;

	let url = runtime::public_hostname(&application.slug, &server);

	Ok(Json(ApplicationResponse {
		id: application.id.0,
		project_id: application.project_id.0,
		server_id: application.server_id.0,
		name: application.name,
		slug: application.slug,
		repo_url: application.repo_url,
		git_ref: application.git_ref,
		build_strategy: application.build_strategy.as_str().to_string(),
		dockerfile_path: application.dockerfile_path,
		container_port: application.container_port,
		memory_limit_mb: application.memory_limit_mb,
		cpu_limit: application.cpu_limit,
		url,
	}))
}
