use axum::Json;
use axum::extract::{Path, State};
use crypto::generate_id;
use podkit_core::domain::application::{Application, BuildStrategy};
use podkit_core::domain::shared::ids::{ApplicationId, ProjectId, ServerId};
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

#[derive(Deserialize)]
pub struct CreateApplicationRequest {
	pub name: String,
	pub slug: String,
	pub server_id: i64,
	pub repo_url: String,
	#[serde(default = "default_git_ref")]
	pub git_ref: String,
	/// Plaintext on the wire (TLS-protected); encrypted at rest. `None`/empty for public repos.
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
	/// Only ever populated on creation, the plaintext is not recoverable
	/// afterwards (only the encrypted form is stored). Wire this into the
	/// git provider's webhook config to enable `POST /webhooks/deploy/{id}`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub webhook_secret: Option<String>,
}

impl ApplicationResponse {
	fn from_app_and_server(app: Application, server: &podkit_core::domain::server::Server) -> Self {
		let url = runtime::public_hostname(&app.slug, server);
		Self {
			id: app.id.0,
			project_id: app.project_id.0,
			server_id: app.server_id.0,
			name: app.name,
			slug: app.slug,
			repo_url: app.repo_url,
			git_ref: app.git_ref,
			build_strategy: app.build_strategy.as_str().to_string(),
			dockerfile_path: app.dockerfile_path,
			container_port: app.container_port,
			memory_limit_mb: app.memory_limit_mb,
			cpu_limit: app.cpu_limit,
			url,
			webhook_secret: None,
		}
	}
}

/// Creates an application under a project, deployed to one of the team's
/// own servers. Caller must hold `create application` on the team; both the
/// project and the target server must belong to that same team.
pub async fn create_application(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id)): Path<(i64, i64)>,
	Json(body): Json<CreateApplicationRequest>,
) -> Result<Json<ApplicationResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
		"application",
	)
	.await?;

	state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	let server = state
		.servers
		.find_by_id(ServerId(body.server_id))
		.await?
		.filter(|s| s.team_id.0 == team_id)
		.ok_or(ServerError::ServerNotFound)?;

	let deploy_key = body
		.deploy_key
		.filter(|k| !k.is_empty())
		.map(|k| state.secrets.encrypt(&k))
		.transpose()
		.map_err(|e| ServerError::Validation(format!("failed to encrypt deploy key: {e}")))?;

	let webhook_secret_plain = crypto::generate_token(24);
	let webhook_secret_encrypted = state
		.secrets
		.encrypt(&webhook_secret_plain)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt webhook secret: {e}")))?;

	let application = Application::new(
		ApplicationId(generate_id()),
		ProjectId(project_id),
		ServerId(body.server_id),
		body.name,
		body.slug,
		body.repo_url,
		body.git_ref,
		deploy_key,
		BuildStrategy::parse(&body.build_strategy),
		body.dockerfile_path,
		body.container_port,
		Some(webhook_secret_encrypted),
		body.memory_limit_mb,
		body.cpu_limit,
	);
	state.applications.save(&application).await?;

	let mut response = ApplicationResponse::from_app_and_server(application, &server);
	response.webhook_secret = Some(webhook_secret_plain);

	Ok(Json(response))
}
