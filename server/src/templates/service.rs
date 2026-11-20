//! Deploying a template: authorize, scope, fill the variables in, then
//! create an image application or a stack and queue its first deploy.

use std::collections::BTreeMap;

use serde::Serialize;

use super::expr::Os;
use super::render::{Placement, Rendered, render};
use super::{Invalid, Loaded};
use crate::applications::service::AppInput;
use crate::applications::{self, public_hostname};
use crate::auth::permission::{Action, Resource, authorize};
use crate::change::Change;
use crate::error::{AppResult, ServerError};
use crate::ids::{EnvironmentId, ModelId, ProjectId, ServerId, TeamId, UserId};
use crate::servers::Server;
use crate::stacks::compose::{self, Inputs, slug};
use crate::stacks::translate::{self, Domain, Settings};
use crate::state::AppState;
use crate::{deployments, env_vars, environments, links, scope, stacks, validate, volumes};

/// How many names and slugs to try before giving up on a taken one.
const ATTEMPTS: u32 = 8;

/// What a deploy asks for.
#[derive(Debug, Default)]
pub struct DeployInput {
	/// Name of the resource; the template's name when empty.
	pub name: Option<String>,
	/// Answers to the template's `string` variables, and hostnames for its
	/// `${domain}` variables.
	pub inputs: BTreeMap<String, String>,
	/// A podkit model to link to the application ([`Loaded`] with `gateway`).
	pub model: Option<ModelId>,
	/// A saved registry to pull the template's images with.
	pub registry: Option<crate::ids::RegistryId>,
}

/// What a template became.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
	/// An image application.
	Application,
	/// A compose stack.
	Stack,
}

/// The outcome of a deploy. Carries no credentials: only the variables that
/// are not secret.
#[derive(Debug, Clone, Serialize)]
pub struct Deployed {
	/// What was created.
	pub kind: Kind,
	/// Id of the application or the stack.
	pub id: i64,
	/// Its name.
	pub name: String,
	/// Where it will answer.
	pub urls: Vec<String>,
	/// Generated values that are not credentials (hostnames, e-mail
	/// addresses, user names) and the answers given.
	pub values: BTreeMap<String, String>,
	/// The generated credentials the template says the user logs in with,
	/// shown here and nowhere else. Everything else generated stays secret.
	pub credentials: BTreeMap<String, String>,
	/// The deployment (application) or the job (stack) doing the first deploy.
	pub deploy_id: i64,
}

fn invalid(e: Invalid) -> ServerError {
	ServerError::Validation(e.0)
}

/// `203-0-113-7`, or `127-0-0-1` for the local server.
fn dashed_ip(server: &Server) -> String {
	applications::dashed_ip(server.is_local, &server.hostname)
}

fn url_of(s: &AppState, host: &str) -> String {
	match s.config.ingress_port {
		80 => format!("http://{host}"),
		port => format!("http://{host}:{port}"),
	}
}

fn candidate(base: &str, attempt: u32) -> String {
	if attempt == 0 {
		base.to_string()
	} else {
		format!("{base} {}", attempt + 1)
	}
}

fn project_name(name: &str) -> AppResult<String> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > 60 {
		return Err(ServerError::Validation(
			"name must be 1 to 60 characters".to_string(),
		));
	}
	Ok(name.to_string())
}

/// Deploys `template` into an environment of a project, on one of the team's
/// servers. Caller must hold `create` on every resource the result is made of.
///
/// A template that is one service with one domain, and nothing an application
/// cannot express, becomes an image application; every other becomes a stack.
///
/// # Errors
/// `NotFound` for an unknown template or another team's project, server or
/// environment; `Validation` for answers the template refuses; `Forbidden`.
#[allow(clippy::too_many_arguments)]
pub async fn deploy(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: Option<EnvironmentId>,
	server: ServerId,
	template: &str,
	body: DeployInput,
) -> AppResult<Deployed> {
	scope::project(&s.db, team, project).await?;
	let environment = environments::service::resolve(&s.db, team, project, environment).await?;
	let server = scope::server(&s.db, team, server).await?;
	let loaded = super::find(template).ok_or(ServerError::NotFound)?;
	let as_app = loaded.plain_app;
	if as_app {
		for resource in [
			Resource::Application,
			Resource::EnvVar,
			Resource::Volume,
			Resource::Deployment,
		] {
			authorize(&s.db, user, team, Action::Create, resource).await?;
		}
		if !body.inputs.is_empty() {
			authorize(&s.db, user, team, Action::Create, Resource::Domain).await?;
		}
	} else {
		authorize(&s.db, user, team, Action::Create, Resource::Stack).await?;
		authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	}
	if body.model.is_some() && !(loaded.template.gateway && as_app) {
		return Err(ServerError::Validation(
			"this template cannot be linked to a model".to_string(),
		));
	}
	let name = project_name(body.name.as_deref().unwrap_or(&loaded.template.name))?;

	if as_app {
		deploy_app(
			s,
			user,
			team,
			project,
			environment,
			&server,
			loaded,
			&name,
			body,
		)
		.await
	} else {
		deploy_stack(
			s,
			user,
			team,
			project,
			environment,
			&server,
			loaded,
			&name,
			body,
		)
		.await
	}
}

#[allow(clippy::too_many_arguments)]
async fn deploy_stack(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: EnvironmentId,
	server: &Server,
	loaded: &Loaded,
	name: &str,
	body: DeployInput,
) -> AppResult<Deployed> {
	let t = &loaded.template;
	let ip = dashed_ip(server);
	let mut last = ServerError::Conflict("could not find a free name".to_string());
	for attempt in 0..ATTEMPTS {
		let name = candidate(name, attempt);
		let place = Placement {
			slug: &slug(&name),
			ip: &ip,
			flat: false,
		};
		let rendered = render(t, &body.inputs, &place, &mut Os).map_err(invalid)?;
		for d in &rendered.domains {
			if !validate::hostname(&d.host) {
				return Err(ServerError::Validation(format!(
					"{:?} is not a valid hostname",
					d.host
				)));
			}
			if stacks::hostname_in_use(&s.db, &d.host, crate::ids::StackServiceId(0)).await? {
				last = ServerError::DomainTaken;
				if body.inputs.is_empty() {
					continue;
				}
				return Err(last);
			}
		}
		let input = stacks::service::CreateInput {
			name: name.clone(),
			compose_yaml: loaded.compose.to_string(),
			env: rendered.env.clone(),
			files: rendered.files.clone(),
			server_id: server.id,
			environment_id: Some(environment),
			source: Some(format!("template:{}@{}", t.id, t.version)),
			deploy: false,
			registry: body.registry,
		};
		let stack = match stacks::service::create(s, user, team, project, input).await {
			Ok((stack, _)) => stack,
			Err(e @ ServerError::Conflict(_)) => {
				last = e;
				continue;
			}
			Err(e) => return Err(e),
		};
		let result = finish_stack(s, user, team, project, &stack, &rendered).await;
		return match result {
			Ok(job) => Ok(Deployed {
				kind: Kind::Stack,
				id: stack.id.0,
				name: stack.name,
				urls: rendered
					.domains
					.iter()
					.map(|d| url_of(s, &d.host))
					.collect(),
				values: rendered.values,
				credentials: rendered.reveal,
				deploy_id: job.0,
			}),
			Err(e) => {
				if let Err(clean) =
					stacks::service::remove(s, user, team, project, stack.id, true).await
				{
					tracing::warn!(
						stack = stack.id.0,
						"failed to clean up a half-made stack: {clean}"
					);
				}
				Err(e)
			}
		};
	}
	Err(last)
}

async fn finish_stack(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	stack: &stacks::Stack,
	rendered: &Rendered,
) -> AppResult<crate::ids::JobId> {
	for d in &rendered.domains {
		stacks::service::set_domain(
			s,
			user,
			team,
			project,
			stack.id,
			&d.service,
			&d.host,
			Some(d.port),
		)
		.await?;
	}
	let (_, job) = stacks::service::deploy(s, user, team, project, stack.id).await?;
	Ok(job)
}

#[allow(clippy::too_many_arguments)]
async fn deploy_app(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: EnvironmentId,
	server: &Server,
	loaded: &Loaded,
	name: &str,
	body: DeployInput,
) -> AppResult<Deployed> {
	let t = &loaded.template;
	let ip = dashed_ip(server);
	let mut last = ServerError::ApplicationSlugTaken;
	for attempt in 0..ATTEMPTS {
		let name = candidate(name, attempt);
		let app_slug = if attempt == 0 {
			t.id.clone()
		} else {
			format!("{}-{}", t.id, attempt + 1)
		};
		if !validate::slug(&app_slug) {
			return Err(ServerError::Validation(
				"template id is not a slug".to_string(),
			));
		}
		let place = Placement {
			slug: &app_slug,
			ip: &ip,
			flat: true,
		};
		let rendered = render(t, &body.inputs, &place, &mut Os).map_err(invalid)?;
		let plan = plan_of(s, loaded, &rendered)?;
		let spec = &plan.services[0].spec;
		let Some(port) = spec.container_port else {
			return Err(ServerError::Internal);
		};
		let proto::Source::Image { reference, .. } = &spec.source else {
			return Err(ServerError::Internal);
		};
		let input = AppInput {
			name: name.clone(),
			slug: app_slug.clone(),
			repo_url: String::new(),
			image: Some(reference.clone()),
			registry_username: None,
			registry_password: None,
			registry: body.registry.map_or(Change::Keep, Change::Set),
			git_ref: "main".to_string(),
			deploy_key: None,
			deploy_key_id: None,
			generate_deploy_key: false,
			git_token: None,
			build_strategy: "dockerfile".to_string(),
			dockerfile_path: "Dockerfile".to_string(),
			container_port: i32::from(port),
			memory_limit_mb: None,
			cpu_limit: None,
			environment_id: Some(environment),
			git_provider_id: None,
			git_repo: String::new(),
		};
		let created =
			match applications::service::create(s, user, team, project, server.id, input).await {
				Ok(created) => created,
				Err(e @ (ServerError::ApplicationSlugTaken | ServerError::Conflict(_))) => {
					last = e;
					continue;
				}
				Err(e) => return Err(e),
			};
		let app = created.app;
		let finished = finish_app(
			s, user, team, project, server, &app.slug, app.id, &plan, &rendered, body.model,
		)
		.await;
		return match finished {
			Ok(deployment) => Ok(Deployed {
				kind: Kind::Application,
				id: app.id.0,
				name: app.name,
				urls: urls_of(s, server, &app.slug, &rendered),
				values: rendered.values,
				credentials: rendered.reveal,
				deploy_id: deployment,
			}),
			Err(e) => {
				if let Err(clean) =
					applications::service::remove(s, user, team, project, app.id).await
				{
					tracing::warn!(
						application = app.id.0,
						"failed to clean up a half-made app: {clean}"
					);
				}
				Err(e)
			}
		};
	}
	Err(last)
}

fn urls_of(s: &AppState, server: &Server, slug: &str, rendered: &Rendered) -> Vec<String> {
	let mut urls = vec![url_of(s, &public_hostname(slug, server))];
	for d in &rendered.domains {
		let url = url_of(s, &d.host);
		if !urls.contains(&url) {
			urls.push(url);
		}
	}
	urls
}

#[allow(clippy::too_many_arguments)]
async fn finish_app(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	server: &Server,
	slug: &str,
	app: crate::ids::ApplicationId,
	plan: &translate::Plan,
	rendered: &Rendered,
	model: Option<ModelId>,
) -> AppResult<i64> {
	let spec = &plan.services[0].spec;
	for (key, value) in &spec.env {
		env_vars::service::upsert(s, user, team, project, app, key, value).await?;
	}
	for volume in &spec.volumes {
		volumes::service::create(&s.db, user, team, project, app, &volume.path).await?;
	}
	// A hostname the user chose, or one that is not the application's own.
	let own = public_hostname(slug, server);
	for d in &rendered.domains {
		if d.host != own {
			crate::domains::service::create(&s.db, user, team, project, app, &d.host).await?;
		}
	}
	if let Some(model) = model {
		links::service::link(s, user, team, project, app, model, true).await?;
	}
	let deployment = deployments::service::deploy(s, user, team, project, app).await?;
	Ok(deployment.id.0)
}

/// The translation of an application template, for the one service.
fn plan_of(s: &AppState, loaded: &Loaded, rendered: &Rendered) -> AppResult<translate::Plan> {
	let parsed = compose::parse(
		&loaded.compose,
		&Inputs {
			env: &rendered.env,
			files: &rendered.files,
		},
	)
	.map_err(|_| ServerError::Internal)?;
	let domains: BTreeMap<String, Domain> = rendered
		.domains
		.iter()
		.map(|d| {
			(
				d.service.clone(),
				Domain {
					host: d.host.clone(),
					port: Some(d.port),
				},
			)
		})
		.collect();
	translate::plan(
		&parsed,
		&Settings {
			stack: 0,
			ingress: proto::Ingress {
				http_port: s.config.ingress_port,
				https_port: s.config.https_port,
				acme_email: s.config.acme_email.clone(),
			},
			domains: &domains,
		},
	)
	.map_err(|_| ServerError::Internal)
}
