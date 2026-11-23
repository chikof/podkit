//! Application use cases shared by the JSON API and the web UI: authorize,
//! then scope, then validate, then touch the database or the agent.
//!
//! Nothing here hands out the encrypted credentials: callers get an
//! [`ApplicationView`], which only says whether one is stored.

use axum::http::Method;
use sqlx::PgPool;
use tracing::warn;

use super::{
	Application, ApplicationUpdate, BuildStrategy, NewApplication, RegistryChoice, SLUG_CONSTRAINT,
	SourceKind, StoredRegistryAuth, public_hostname,
};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::change::Change;
use crate::deployments::{self, DeploymentStatus};
use crate::error::{AppResult, ServerError};
use crate::git_providers::service as git_service;
use crate::ids::{
	ApplicationId, EnvironmentId, GitProviderId, ProjectId, RegistryId, ServerId, SshKeyId, TeamId,
	UserId,
};
use crate::registries;
use crate::servers::Server;
use crate::ssh_keys::service::{self as ssh_key_service, KeyChoice};
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
	/// `None` for image applications.
	pub repo_url: Option<String>,
	pub source_kind: SourceKind,
	/// `None` for git applications.
	pub image: Option<String>,
	pub git_ref: String,
	pub build_strategy: BuildStrategy,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
	/// The generated public hostname.
	pub url: String,
	pub has_deploy_key: bool,
	/// The vault key used as the deploy key.
	pub deploy_key_id: Option<SshKeyId>,
	pub has_git_token: bool,
	/// The git provider the repository is cloned through, and the repository there.
	pub git_provider_id: Option<GitProviderId>,
	pub git_repo: Option<String>,
	/// Whether registry credentials are stored (their contents never leave
	/// the server).
	pub has_registry_auth: bool,
	/// The saved registry the image is pulled with.
	pub registry_id: Option<RegistryId>,
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
			source_kind: app.source_kind,
			image: app.image.clone(),
			git_ref: app.git_ref.clone(),
			build_strategy: app.build_strategy,
			dockerfile_path: app.dockerfile_path.clone(),
			container_port: app.container_port,
			memory_limit_mb: app.memory_limit_mb,
			cpu_limit: app.cpu_limit,
			url: public_hostname(&app.slug, server),
			has_deploy_key: app.deploy_key_id.is_some(),
			deploy_key_id: app.deploy_key_id,
			has_git_token: app.git_token.is_some(),
			has_registry_auth: app.registry_auth.is_some() || app.registry_id.is_some(),
			registry_id: app.registry_id,
			git_provider_id: app.git_provider_id,
			git_repo: app.git_repo.clone(),
			latest,
		}
	}
}

/// What a create or an update sets. Either `repo_url` (git) or `image` is
/// set, never both. `deploy_key`, `git_token` and the registry credentials
/// are write-only: on update, `None` or empty keeps the stored value.
#[derive(Debug, Clone)]
pub struct AppInput {
	pub name: String,
	pub slug: String,
	/// Empty for image applications.
	pub repo_url: String,
	/// Registry reference; `None` or empty for git applications.
	pub image: Option<String>,
	pub registry_username: Option<String>,
	pub registry_password: Option<String>,
	/// A saved registry to pull with: `None` leaves it as it is (on update), `Some(None)`
	/// clears it, `Some(Some(id))` uses that one.
	pub registry: Change<RegistryId>,
	pub git_ref: String,
	/// A pasted SSH private key, imported into the vault under the application's name.
	pub deploy_key: Option<String>,
	/// A key from the vault.
	pub deploy_key_id: Option<SshKeyId>,
	/// A new Ed25519 key, made for this application.
	pub generate_deploy_key: bool,
	pub git_token: Option<String>,
	pub build_strategy: String,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f64>,
	/// Create only: the environment to put the application in. The project's first
	/// environment when `None`; ignored by update.
	pub environment_id: Option<EnvironmentId>,
	/// Create only: clone `git_repo` through this git provider. Then `repo_url`, the deploy
	/// key and the token must be empty: the provider supplies all three.
	pub git_provider_id: Option<GitProviderId>,
	/// `owner/name` at the provider.
	pub git_repo: String,
}

/// A freshly created application and the only copy of its webhook secret.
pub struct Created {
	pub app: ApplicationView,
	pub webhook_secret: String,
	/// Set when the push webhook of a provider application is not in place: why, and what to do.
	pub notice: Option<String>,
}

/// The validated source half of an [`AppInput`].
enum ResolvedSource {
	Git {
		repo_url: String,
	},
	Image {
		image: String,
		/// `(username, password)` when both were given.
		registry: Option<(String, String)>,
	},
}

impl ResolvedSource {
	fn kind(&self) -> SourceKind {
		match self {
			Self::Git { .. } => SourceKind::Git,
			Self::Image { .. } => SourceKind::Image,
		}
	}
}

impl AppInput {
	/// Whether any deploy key was asked for: a vault key, a pasted one, or a new one.
	fn has_key(&self) -> bool {
		self.deploy_key_id.is_some()
			|| self.generate_deploy_key
			|| given(self.deploy_key.as_deref()).is_some()
	}

	fn key_choice(&self) -> AppResult<Option<KeyChoice>> {
		KeyChoice::from_request(
			self.deploy_key_id,
			self.deploy_key.clone(),
			self.generate_deploy_key,
		)
	}
}

/// The source of an application that clones through a provider: the provider supplies the
/// URL and the credential, so none may be given as well.
fn provider_source(body: &AppInput, repo_url: String) -> AppResult<ResolvedSource> {
	if body.has_key()
		|| given(body.git_token.as_deref()).is_some()
		|| given(body.image.as_deref()).is_some()
		|| given(body.registry_username.as_deref()).is_some()
		|| given(body.registry_password.as_deref()).is_some()
	{
		return Err(ServerError::Validation(
			"An application that clones through a git provider takes no deploy key, token or image."
				.to_owned(),
		));
	}
	if !validate::rel_path(&body.dockerfile_path) {
		return Err(ServerError::Validation(
			"invalid dockerfile_path".to_string(),
		));
	}
	Ok(ResolvedSource::Git { repo_url })
}

fn given(v: Option<&str>) -> Option<&str> {
	v.map(str::trim).filter(|v| !v.is_empty())
}

/// Picks and validates the source: exactly one of `repo_url` and `image`,
/// the same one the application already has on update, with only the
/// credentials that fit it.
fn resolve_source(body: &AppInput, existing: Option<SourceKind>) -> AppResult<ResolvedSource> {
	let repo_url = body.repo_url.as_str();
	let image = given(body.image.as_deref());
	let resolved = match (repo_url.is_empty(), image) {
		(false, Some(_)) => {
			return Err(ServerError::Validation(
				"set either repo_url or image, not both".to_string(),
			));
		}
		(true, None) => {
			return Err(ServerError::Validation(
				"repo_url or image is required".to_string(),
			));
		}
		(false, None) => {
			validate_source(
				repo_url,
				&body.dockerfile_path,
				body.has_key(),
				body.git_token.as_deref(),
			)?;
			if given(body.registry_username.as_deref()).is_some()
				|| given(body.registry_password.as_deref()).is_some()
			{
				return Err(ServerError::Validation(
					"registry credentials only apply to image applications".to_string(),
				));
			}
			ResolvedSource::Git {
				repo_url: repo_url.to_owned(),
			}
		}
		(true, Some(image)) => {
			if !validate::image_ref(image) {
				return Err(ServerError::Validation(
					"invalid image reference".to_string(),
				));
			}
			if body.has_key() || given(body.git_token.as_deref()).is_some() {
				return Err(ServerError::Validation(
					"deploy_key and git_token only apply to git applications".to_string(),
				));
			}
			let registry = match (
				given(body.registry_username.as_deref()),
				body.registry_password.as_deref().filter(|p| !p.is_empty()),
			) {
				(Some(user), Some(password)) => Some((user.to_owned(), password.to_owned())),
				(None, None) => None,
				_ => {
					return Err(ServerError::Validation(
						"registry_username and registry_password go together".to_string(),
					));
				}
			};
			ResolvedSource::Image {
				image: image.to_owned(),
				registry,
			}
		}
	};
	if existing.is_some_and(|kind| kind != resolved.kind()) {
		return Err(ServerError::Validation(
			"an application's source type cannot be changed".to_string(),
		));
	}
	Ok(resolved)
}

/// Checks the repo URL, Dockerfile path, and that each credential matches
/// the URL scheme: deploy keys are SSH-only, tokens are HTTPS-only.
fn validate_source(
	repo_url: &str,
	dockerfile_path: &str,
	deploy_key: bool,
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
	if deploy_key && https {
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

/// Splits a validated source into the columns it fills. Registry
/// credentials are sealed as JSON; `None` keeps the stored ones on update.
#[allow(clippy::type_complexity)]
fn split_source(
	state: &AppState,
	source: ResolvedSource,
) -> AppResult<(Option<String>, Option<String>, Option<Vec<u8>>)> {
	match source {
		ResolvedSource::Git { repo_url } => Ok((Some(repo_url), None, None)),
		ResolvedSource::Image { image, registry } => {
			let sealed = registry
				.map(|(username, password)| {
					let json = serde_json::to_string(&StoredRegistryAuth { username, password })
						.map_err(|e| {
							ServerError::Validation(format!("failed to encode registry auth: {e}"))
						})?;
					state.secrets.encrypt(&json).map_err(|e| {
						ServerError::Validation(format!("failed to encrypt registry auth: {e}"))
					})
				})
				.transpose()?;
			Ok((None, Some(image), sealed))
		}
	}
}

#[allow(clippy::cast_possible_truncation)]
fn cpu_to_f32(cpu: Option<f64>) -> Option<f32> {
	cpu.map(|c| c as f32)
}

/// Settles which saved registry the application pulls with after this input. A saved
/// registry must be the team's own and the one the image comes from, so its login is never
/// sent to another host; an inline login replaces a saved registry.
async fn choose_registry(
	db: &PgPool,
	team: TeamId,
	body: &AppInput,
	image: Option<&str>,
	existing: Option<&Application>,
) -> AppResult<RegistryChoice> {
	let inline = given(body.registry_username.as_deref()).is_some()
		|| given(body.registry_password.as_deref()).is_some();
	match (body.registry, image) {
		(Change::Set(_), None) => Err(ServerError::Validation(
			"a registry only applies to image applications".to_string(),
		)),
		(Change::Set(_), _) if inline => Err(ServerError::Validation(
			"pick a saved registry or enter a login, not both".to_string(),
		)),
		(Change::Set(id), Some(image)) => {
			registries::service::pick_for_image(db, team, id, image).await?;
			Ok(RegistryChoice::Use(id))
		}
		(Change::Clear, _) => Ok(RegistryChoice::Clear),
		(Change::Keep, _) if inline => Ok(RegistryChoice::Clear),
		(Change::Keep, Some(image)) => {
			if let Some(id) = existing.and_then(|a| a.registry_id) {
				registries::service::pick_for_image(db, team, id, image).await?;
			}
			Ok(RegistryChoice::Keep)
		}
		(Change::Keep, None) => Ok(RegistryChoice::Keep),
	}
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
#[allow(
	clippy::too_many_lines,
	reason = "the merged cards each added a step; one flow reads better unsplit"
)]
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
	let (source, provider) = match body.git_provider_id {
		Some(id) => {
			if !body.repo_url.trim().is_empty() {
				return Err(ServerError::Validation(
					"Set either a git provider or a repo_url, not both.".to_owned(),
				));
			}
			let resolved = git_service::resolve_repo(s, user, team, id, &body.git_repo).await?;
			(
				provider_source(&body, resolved.repo_url.clone())?,
				Some(resolved),
			)
		}
		None => (resolve_source(&body, None)?, None),
	};
	let key = match body.key_choice()? {
		Some(choice) => Some(
			ssh_key_service::resolve(
				s,
				user,
				team,
				choice,
				&format!("Deploy key for {}", body.name.trim()),
			)
			.await?,
		),
		None => None,
	};
	let git_token = seal(s, body.git_token.as_deref(), "git token")?;
	let (repo_url, image, registry_auth) = split_source(s, source)?;
	let registry = choose_registry(&s.db, team, &body, image.as_deref(), None).await?;

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
			source_kind: if image.is_some() {
				SourceKind::Image
			} else {
				SourceKind::Git
			},
			repo_url,
			image,
			registry_auth,
			registry_id: match registry {
				RegistryChoice::Use(id) => Some(id),
				_ => None,
			},
			git_ref: body.git_ref,
			deploy_key_id: key.map(|k| k.id),
			git_token,
			build_strategy: BuildStrategy::parse(&body.build_strategy),
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			webhook_secret: Some(webhook_secret),
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: cpu_to_f32(body.cpu_limit),
			git_provider_id: provider.as_ref().map(|p| p.provider),
			git_repo: provider.map(|p| p.repo),
		},
	)
	.await
	.map_err(slug_conflict);
	let app = match app {
		Ok(app) => app,
		Err(e) => {
			if let Some(key) = key {
				ssh_key_service::discard(s, team, key).await;
			}
			return Err(e);
		}
	};

	let notice = if app.git_provider_id.is_some() {
		git_service::after_app_created(s, &app, &webhook_secret_plain)
			.await
			.notice()
	} else {
		None
	};
	audit::log(
		&s.db,
		user,
		team,
		"app.create",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set("name", app.name.clone())
			.set("slug", app.slug.clone())
			.set("server_id", server.id.0)
			.set("source", app.source_kind.as_str())
			.set("image", app.image.clone())
			.set("repo_url", app.repo_url.as_deref().map(audit::scrub_url))
			.set("git_ref", app.git_ref.clone())
			.secret("webhook_secret"),
	)
	.await;
	Ok(Created {
		app: ApplicationView::new(&app, &server, None),
		webhook_secret: webhook_secret_plain,
		notice,
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
#[allow(
	clippy::too_many_lines,
	reason = "the audit diff lists each field that can change"
)]
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
	let source = if app.git_provider_id.is_some() {
		// The provider and repository are fixed; the form's URL is only a display of them.
		provider_source(&body, app.repo_url.clone().unwrap_or_default())?
	} else {
		resolve_source(&body, Some(app.source_kind))?
	};
	let key = match body.key_choice()? {
		Some(choice) => Some(
			ssh_key_service::resolve(
				s,
				user,
				team,
				choice,
				&format!("Deploy key for {}", body.name.trim()),
			)
			.await?,
		),
		None => None,
	};
	let git_token = seal(s, body.git_token.as_deref(), "git token")?;
	let (repo_url, image, registry_auth) = split_source(s, source)?;
	let registry = choose_registry(&s.db, team, &body, image.as_deref(), Some(&app)).await?;

	let updated = super::update(
		&s.db,
		app.id,
		ApplicationUpdate {
			name: body.name.trim().to_owned(),
			slug: body.slug,
			repo_url,
			image,
			registry_auth,
			registry,
			git_ref: body.git_ref,
			deploy_key_id: key.map(|k| k.id),
			git_token,
			build_strategy: BuildStrategy::parse(&body.build_strategy),
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			memory_limit_mb: body.memory_limit_mb,
			cpu_limit: cpu_to_f32(body.cpu_limit),
		},
	)
	.await
	.map_err(slug_conflict);
	let updated = match updated {
		Ok(updated) => updated,
		Err(e) => {
			if let Some(key) = key {
				ssh_key_service::discard(s, team, key).await;
			}
			return Err(e);
		}
	};

	audit::log(
		&s.db,
		user,
		team,
		"app.update",
		Target::new("application", updated.id)
			.named(&updated.slug)
			.in_project(project),
		Diff::new()
			.changed("name", app.name.clone(), updated.name.clone())
			.changed("slug", app.slug.clone(), updated.slug.clone())
			.changed("image", app.image.clone(), updated.image.clone())
			.changed(
				"repo_url",
				app.repo_url.as_deref().map(audit::scrub_url),
				updated.repo_url.as_deref().map(audit::scrub_url),
			)
			.changed("git_ref", app.git_ref.clone(), updated.git_ref.clone())
			.changed("container_port", app.container_port, updated.container_port)
			.changed(
				"dockerfile_path",
				app.dockerfile_path.clone(),
				updated.dockerfile_path.clone(),
			)
			.changed(
				"memory_limit_mb",
				app.memory_limit_mb,
				updated.memory_limit_mb,
			)
			.changed(
				"build_strategy",
				app.build_strategy.as_str(),
				updated.build_strategy.as_str(),
			)
			.changed(
				"deploy_key_id",
				app.deploy_key_id.map(|k| k.0),
				updated.deploy_key_id.map(|k| k.0),
			)
			.changed(
				"registry_id",
				app.registry_id.map(|k| k.0),
				updated.registry_id.map(|k| k.0),
			),
	)
	.await;
	Ok(ApplicationView::new(&updated, &server, None))
}

/// Replaces the webhook secret and returns the new one in plaintext: the only time it is
/// ever shown. Whatever used the old secret stops working at once. Needs `update
/// application`; another team's application is `ApplicationNotFound`.
pub async fn rotate_webhook_secret(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<String> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, id).await?;
	let plain = crypto::generate_token(24);
	let sealed = s
		.secrets
		.encrypt(&plain)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt webhook secret: {e}")))?;
	super::set_webhook_secret(&s.db, app.id, &sealed).await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.webhook_secret.rotate",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new().secret("webhook_secret"),
	)
	.await;
	Ok(plain)
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
	crate::mcp_servers::service::ensure_removable(&s.db, app.id).await?;
	remove_service(s, team, &app).await;
	crate::tailscale::service::before_app_removed(s, team, &app).await;
	git_service::before_app_removed(s, &app).await;
	super::delete(&s.db, app.id).await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.delete",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set("name", app.name.clone())
			.set("slug", app.slug.clone()),
	)
	.await;
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
