//! Application use cases shared by the JSON API and the web UI: authorize,
//! then scope, then validate, then touch the database or the agent.
//!
//! Nothing here hands out the encrypted credentials: callers get an
//! [`ApplicationView`], which only says whether one is stored.

use axum::http::Method;
use sqlx::PgPool;
use tracing::warn;

use super::{
	Application, ApplicationUpdate, BuildStrategy, NewApplication, SLUG_CONSTRAINT, public_hostname,
};
use crate::auth::permission::{Action, Resource, authorize};
use crate::deployments::{self, DeploymentStatus};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, ServerId, TeamId, UserId};
use crate::servers::Server;
use crate::state::AppState;
use crate::{db, environments, scope, servers, validate, volumes};

const MAX_NAME_LEN: usize = 60;

/// An application as callers may see it. Carries no secrets.
#[derive(Debug, Clone)]
pub struct ApplicationView {
	pub id: ApplicationId,
	pub project_id: ProjectId,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub server_name: String,
	pub name: String,
	pub slug: String,
	pub repo_url: String,
	pub git_ref: String,
	pub build_strategy: BuildStrategy,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
	/// The generated public hostname.
	pub url: String,
	pub has_deploy_key: bool,
	pub has_git_token: bool,
	/// Status of the newest deployment. Filled in by [`get`] only.
	pub latest: Option<DeploymentStatus>,
}

impl ApplicationView {
	pub(crate) fn new(
		app: &Application,
		server: &Server,
		latest: Option<DeploymentStatus>,
	) -> Self {
		Self {
			id: app.id,
			project_id: app.project_id,
			environment_id: app.environment_id,
			server_id: app.server_id,
			server_name: server.name.clone(),
			name: app.name.clone(),
			slug: app.slug.clone(),
			repo_url: app.repo_url.clone(),
			git_ref: app.git_ref.clone(),
			build_strategy: app.build_strategy,
			dockerfile_path: app.dockerfile_path.clone(),
			container_port: app.container_port,
			memory_limit_mb: app.memory_limit_mb,
			cpu_limit: app.cpu_limit,
			url: public_hostname(&app.slug, server),
			has_deploy_key: app.deploy_key.is_some(),
			has_git_token: app.git_token.is_some(),
			latest,
		}
	}
}

/// What a create or an update sets. `deploy_key` and `git_token` are
/// write-only: on update, `None` or empty keeps the stored value.
#[derive(Debug, Clone)]
pub struct AppInput {
	pub name: String,
	pub slug: String,
	pub repo_url: String,
	pub git_ref: String,
	pub deploy_key: Option<String>,
	pub git_token: Option<String>,
	pub build_strategy: String,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	/// Create only: the environment to put the application in. The project's first
	/// environment when `None`; ignored by update.
	pub environment_id: Option<EnvironmentId>,
}

/// A freshly created application and the only copy of its webhook secret.
pub struct Created {
	pub app: ApplicationView,
	pub webhook_secret: String,
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

fn validate_basics(body: &AppInput) -> AppResult<()> {
	let (name, slug) = (&body.name, &body.slug);
	if !(1..=65535).contains(&body.container_port) {
		return Err(ServerError::Validation(
			"Container port must be between 1 and 65535.".to_owned(),
		));
	}
	if body.memory_limit_mb.is_some_and(|m| m < 1) {
		return Err(ServerError::Validation(
			"Memory limit must be at least 1 MB.".to_owned(),
		));
	}
	if body.cpu_limit.is_some_and(|c| !c.is_finite() || c <= 0.0) {
		return Err(ServerError::Validation(
			"CPU limit must be greater than 0.".to_owned(),
		));
	}
	if name.trim().is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	if !validate::slug(slug) {
		return Err(ServerError::Validation(
			"Slug must be 1 to 60 lowercase letters, digits or hyphens, and not start or end with a hyphen."
				.to_owned(),
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

/// The team's servers, for the create form's server select; needs
/// `create application`.
pub async fn servers_for_create(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Server>> {
	authorize(db, user, team, Action::Create, Resource::Application).await?;
	Ok(servers::list_by_team(db, team).await?)
}

/// Creates an application under a project, deployed to one of the team's
/// own servers. Caller must hold `create application` on the team; both the
/// project and the target server must belong to that same team.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	server_id: ServerId,
	body: AppInput,
) -> AppResult<Created> {
	authorize(&s.db, user, team, Action::Create, Resource::Application).await?;
	scope::project(&s.db, team, project).await?;
	let environment =
		environments::service::resolve(&s.db, team, project, body.environment_id).await?;
	let server = scope::server(&s.db, team, server_id).await?;

	validate_basics(&body)?;
	validate_source(
		&body.repo_url,
		&body.dockerfile_path,
		body.deploy_key.as_deref(),
		body.git_token.as_deref(),
	)?;
	let deploy_key = seal(s, body.deploy_key.as_deref(), "deploy key")?;
	let git_token = seal(s, body.git_token.as_deref(), "git token")?;

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
			environment_id: environment,
			server_id: server.id,
			name: body.name.trim().to_owned(),
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

	Ok(Created {
		app: ApplicationView::new(&app, &server, None),
		webhook_secret: webhook_secret_plain,
	})
}

/// Lists applications of one environment of a project, the project's first
/// when `environment` is `None`; needs `read application`. The project (and
/// the environment, if given) must belong to the team. `latest` is not filled in.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: Option<EnvironmentId>,
) -> AppResult<Vec<ApplicationView>> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	scope::project(db, team, project).await?;
	let environment = environments::service::resolve(db, team, project, environment).await?;

	let apps = super::list_by_environment(db, environment).await?;
	let servers = servers::list_by_team(db, team).await?;
	apps.iter()
		.map(|app| {
			let server = servers
				.iter()
				.find(|s| s.id == app.server_id)
				.ok_or(ServerError::ServerNotFound)?;
			Ok(ApplicationView::new(app, server, None))
		})
		.collect()
}

/// One application, with the status of its newest deployment; needs
/// `read application`. One of another team or project is `ApplicationNotFound`.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<ApplicationView> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(db, team, project, id).await?;
	let server = scope::server(db, team, app.server_id).await?;
	let latest = deployments::list(db, app.id, None, 1)
		.await?
		.first()
		.map(|d| d.status);
	Ok(ApplicationView::new(&app, &server, latest))
}

/// Updates an application's configuration (everything set at creation
/// except the server, which is immutable). Takes effect on the *next*
/// deploy: a running container isn't touched. Needs `update application`.
pub async fn update(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
	body: AppInput,
) -> AppResult<ApplicationView> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, id).await?;
	let server = scope::server(&s.db, team, app.server_id).await?;

	validate_basics(&body)?;
	validate_source(
		&body.repo_url,
		&body.dockerfile_path,
		body.deploy_key.as_deref(),
		body.git_token.as_deref(),
	)?;
	let deploy_key = seal(s, body.deploy_key.as_deref(), "deploy key")?;
	let git_token = seal(s, body.git_token.as_deref(), "git token")?;

	let updated = super::update(
		&s.db,
		app.id,
		ApplicationUpdate {
			name: body.name.trim().to_owned(),
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

	Ok(ApplicationView::new(&updated, &server, None))
}

/// Deletes an application; needs `delete application`. Containers, volumes
/// and images on the server are removed best-effort first: the row is the
/// source of truth, so a failure there never blocks the delete.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<ApplicationId> {
	authorize(&s.db, user, team, Action::Delete, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, id).await?;
	remove_service(s, team, &app).await;
	super::delete(&s.db, app.id).await?;
	Ok(app.id)
}

async fn remove_service(s: &AppState, team: TeamId, app: &Application) {
	let server = match scope::server(&s.db, team, app.server_id).await {
		Ok(server) => server,
		Err(e) => {
			warn!(
				application = app.id.0,
				"cannot load server to remove containers: {e}"
			);
			return;
		}
	};
	let client = match s.agents.client(s, &server).await {
		Ok(client) => client,
		Err(e) => {
			warn!(
				application = app.id.0,
				"cannot reach server agent to remove containers: {e}"
			);
			return;
		}
	};
	if let Err(e) = client
		.send(Method::DELETE, &format!("/v1/services/app-{}", app.id))
		.await
	{
		warn!(
			application = app.id.0,
			"failed to remove application containers: {e}"
		);
	}

	let volumes = match volumes::list(&s.db, app.id).await {
		Ok(volumes) => volumes,
		Err(e) => {
			warn!(application = app.id.0, "cannot list volumes to remove: {e}");
			return;
		}
	};
	for volume in volumes {
		if let Err(e) = client
			.send(Method::DELETE, &format!("/v1/volumes/{}", volume.name))
			.await
		{
			warn!(
				application = app.id.0,
				volume = volume.id.0,
				"failed to remove volume: {e}"
			);
		}
	}
}
