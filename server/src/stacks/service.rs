//! Stack use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or the agent.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::http::Method;
use futures_util::stream::BoxStream;
use proto::{ContainerHealth, ContainerState, Ingress};
use sqlx::PgPool;

use super::compose::{self, Compose, Inputs, Issue, Warning};
use super::translate::{self, Domain, Plan, Settings};
use super::{
	DOMAIN_INDEX, NAME_CONSTRAINT, NewStack, SLUG_CONSTRAINT, ServiceState, ServiceStatus, Stack,
	StackService, StackStatus,
};
use crate::auth::permission::{Action, Resource, authorize};
use crate::change::Change;
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{EnvironmentId, JobId, ProjectId, RegistryId, ServerId, StackId, TeamId, UserId};
use crate::jobs::{self, JobKind, JobStatus};
use crate::state::AppState;
use crate::{environments, registries, scope, servers, stacks, validate};

const MAX_TAIL: u32 = 5000;
const MAX_ENV_VARS: usize = 200;
const MAX_ENV_VALUE_BYTES: usize = 64 * 1024;
/// How long a status refresh may spend on the agent before the page shows
/// what the database says.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(4);

pub struct CreateInput {
	pub name: String,
	pub compose_yaml: String,
	pub env: BTreeMap<String, String>,
	/// Files a template mounts, by path relative to the compose file.
	pub files: BTreeMap<String, String>,
	pub server_id: ServerId,
	/// The project's first environment when `None`.
	pub environment_id: Option<EnvironmentId>,
	/// `raw` when `None`.
	pub source: Option<String>,
	/// Queue the first deploy right away.
	pub deploy: bool,
	/// A saved registry to pull the images with.
	pub registry: Option<RegistryId>,
}

/// Changes to a stack; omitted fields stay as they are. Variables are
/// write-only: they can be set and removed, never read back.
#[derive(Default)]
pub struct UpdateInput {
	pub name: Option<String>,
	pub compose_yaml: Option<String>,
	pub env_set: BTreeMap<String, String>,
	pub env_unset: Vec<String>,
	/// `None` leaves the saved registry as it is, `Some(None)` clears it.
	pub registry: Change<RegistryId>,
}

/// A stack with what a page about it shows. Carries no variable values.
#[derive(Debug, Clone)]
pub struct StackDetail {
	pub stack: Stack,
	pub services: Vec<StackService>,
	/// Names of the variables set on the stack, never their values.
	pub env_keys: Vec<String>,
	/// Keys the translation ignores or approximates.
	pub warnings: Vec<Warning>,
	/// Variables the compose file uses that the stack does not set.
	pub missing_vars: Vec<String>,
	/// The newest job of the stack, if any ever ran.
	pub job: Option<(JobId, JobStatus)>,
}

fn unprocessable(message: &str, issues: &[Issue]) -> ServerError {
	ServerError::Unprocessable {
		message: message.to_string(),
		issues: serde_json::to_value(issues).unwrap_or_default(),
	}
}

fn unique_conflict(e: sqlx::Error) -> ServerError {
	if is_unique_violation(&e, NAME_CONSTRAINT) || is_unique_violation(&e, SLUG_CONSTRAINT) {
		ServerError::Conflict("a stack with this name already exists in this project".to_string())
	} else if is_unique_violation(&e, DOMAIN_INDEX) {
		ServerError::DomainTaken
	} else {
		e.into()
	}
}

fn ingress(s: &AppState) -> Ingress {
	Ingress {
		http_port: s.config.ingress_port,
		https_port: s.config.https_port,
		acme_email: s.config.acme_email.clone(),
	}
}

fn domains_of(services: &[StackService]) -> BTreeMap<String, Domain> {
	services
		.iter()
		.filter(|svc| svc.status != ServiceStatus::Removing)
		.filter_map(|svc| {
			svc.domain.as_ref().map(|host| {
				(
					svc.name.clone(),
					Domain {
						host: host.clone(),
						port: svc.domain_port.and_then(|p| u16::try_from(p).ok()),
					},
				)
			})
		})
		.collect()
}

/// Parses `yaml` against `env` and translates it for `stack`, so a request
/// that could never deploy is refused with the offending keys instead of
/// failing a job later.
///
/// # Errors
/// `Unprocessable` listing each refused key with its service and path.
pub fn check(
	s: &AppState,
	stack: StackId,
	yaml: &str,
	env: &BTreeMap<String, String>,
	files: &BTreeMap<String, String>,
	domains: &BTreeMap<String, Domain>,
) -> AppResult<(Compose, Plan)> {
	let compose = compose::parse(yaml, &Inputs { env, files })
		.map_err(|issues| unprocessable("the compose file uses unsupported features", &issues))?;
	let plan = translate::plan(
		&compose,
		&Settings {
			stack: stack.0,
			ingress: ingress(s),
			domains,
		},
	)
	.map_err(|issues| unprocessable("the stack cannot be deployed as configured", &issues))?;
	Ok((compose, plan))
}

/// Checks a compose file as a new stack of `project` would be checked, creating
/// nothing: the warnings and the variables it lacks when it would deploy, the offending keys when not. The
/// web UI's validation panel. Caller must hold `create stack`.
///
/// # Errors
/// `Unprocessable` listing each refused key with its service and path.
pub async fn validate(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	yaml: &str,
	env: &BTreeMap<String, String>,
) -> AppResult<(Vec<Warning>, Vec<String>)> {
	authorize(&s.db, user, team, Action::Create, Resource::Stack).await?;
	scope::project(&s.db, team, project).await?;
	validate_env(env)?;
	let (compose, _) = check(
		s,
		StackId::generate(),
		yaml,
		env,
		&BTreeMap::new(),
		&BTreeMap::new(),
	)?;
	Ok((compose.warnings, compose.missing_vars.into_iter().collect()))
}

fn validate_name(name: &str) -> AppResult<String> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > 60 {
		return Err(ServerError::Validation(
			"name must be 1 to 60 characters".to_string(),
		));
	}
	let slug = compose::slug(name);
	if slug.is_empty() {
		return Err(ServerError::Validation(
			"name needs at least one letter or digit".to_string(),
		));
	}
	Ok(slug)
}

fn validate_env(env: &BTreeMap<String, String>) -> AppResult<()> {
	if env.len() > MAX_ENV_VARS {
		return Err(ServerError::Validation(format!(
			"a stack has at most {MAX_ENV_VARS} variables"
		)));
	}
	for (key, value) in env {
		let ok = key.len() <= 128
			&& key
				.chars()
				.next()
				.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
			&& key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
		if !ok {
			return Err(ServerError::Validation(format!(
				"invalid variable name {key:?}: use letters, digits and _"
			)));
		}
		if value.len() > MAX_ENV_VALUE_BYTES {
			return Err(ServerError::Validation(format!(
				"variable {key} is longer than {} KiB",
				MAX_ENV_VALUE_BYTES / 1024
			)));
		}
	}
	Ok(())
}

fn images_of(compose: &Compose) -> BTreeMap<String, String> {
	compose
		.services
		.iter()
		.map(|(name, svc)| (name.clone(), svc.image.clone()))
		.collect()
}

fn files_of(s: &AppState, stack: &Stack) -> AppResult<BTreeMap<String, String>> {
	stacks::decrypt_files(&s.secrets, stack).map_err(|e| {
		tracing::error!(stack = stack.id.0, "{e}");
		ServerError::Internal
	})
}

fn env_of(s: &AppState, stack: &Stack) -> AppResult<BTreeMap<String, String>> {
	stacks::decrypt_env(&s.secrets, stack).map_err(|e| {
		tracing::error!(stack = stack.id.0, "{e}");
		ServerError::Internal
	})
}

/// Queues `kind` for `stack`; if that fails the row is marked failed, so it
/// doesn't sit in a state no job will ever move it out of.
async fn queue_or_fail(
	s: &AppState,
	kind: JobKind,
	stack: &Stack,
	target: i64,
	message: &str,
) -> AppResult<JobId> {
	match stacks::enqueue(s, kind, stack, target).await {
		Ok(job) => Ok(job),
		Err(e) => {
			if let Err(mark) =
				stacks::set_status(&s.db, stack.id, StackStatus::Failed, Some(message)).await
			{
				tracing::warn!(
					stack = stack.id.0,
					"failed to mark unqueued stack failed: {mark}"
				);
			}
			Err(e)
		}
	}
}

/// Creates a stack from a compose file and, with `deploy`, queues its first
/// deploy. Caller must hold `create stack` on the team.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	body: CreateInput,
) -> AppResult<(Stack, Option<JobId>)> {
	authorize(&s.db, user, team, Action::Create, Resource::Stack).await?;
	scope::project(&s.db, team, project).await?;
	let environment =
		environments::service::resolve(&s.db, team, project, body.environment_id).await?;
	let server = scope::server(&s.db, team, body.server_id).await?;
	let slug = validate_name(&body.name)?;
	validate_env(&body.env)?;
	if let Some(registry) = body.registry {
		registries::service::pick(&s.db, team, registry).await?;
	}

	let id = StackId::generate();
	let (compose, _) = check(
		s,
		id,
		&body.compose_yaml,
		&body.env,
		&body.files,
		&BTreeMap::new(),
	)?;
	let source = body.source.unwrap_or_else(|| "raw".to_string());
	if source.len() > 120 {
		return Err(ServerError::Validation("source is too long".to_string()));
	}

	let stack = stacks::insert(
		&s.db,
		NewStack {
			id,
			project_id: project,
			environment_id: environment,
			server_id: server.id,
			name: body.name.trim().to_string(),
			slug,
			compose_yaml: body.compose_yaml,
			env: stacks::encrypt_env(&s.secrets, &body.env)?,
			files: if body.files.is_empty() {
				Vec::new()
			} else {
				stacks::encrypt_env(&s.secrets, &body.files)?
			},
			source,
			registry_id: body.registry,
		},
	)
	.await
	.map_err(unique_conflict)?;
	stacks::sync_services(&s.db, stack.id, &images_of(&compose)).await?;

	let job = if body.deploy {
		Some(start_deploy(s, &stack).await?)
	} else {
		None
	};
	Ok((stack, job))
}

/// The stacks of one environment of a project, the first when `environment`
/// is `None`; needs `read stack`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: Option<EnvironmentId>,
) -> AppResult<Vec<Stack>> {
	authorize(db, user, team, Action::Read, Resource::Stack).await?;
	scope::project(db, team, project).await?;
	let environment = environments::service::resolve(db, team, project, environment).await?;
	Ok(stacks::list_by_environment(db, environment).await?)
}

/// One stack without its variables; needs `read stack`.
pub async fn find(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
) -> AppResult<Stack> {
	authorize(db, user, team, Action::Read, Resource::Stack).await?;
	scope::stack(db, team, project, id).await
}

/// A stack with its services, variable names and newest job. The services'
/// state is refreshed from the agent when it answers in time; needs
/// `read stack`.
pub async fn detail(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
) -> AppResult<StackDetail> {
	let stack = find(&s.db, user, team, project, id).await?;
	let mut services = stacks::list_services(&s.db, stack.id).await?;
	if matches!(stack.status, StackStatus::Running | StackStatus::Failed)
		&& tokio::time::timeout(REFRESH_TIMEOUT, refresh(s, &stack, &mut services))
			.await
			.is_err()
	{
		tracing::debug!(stack = stack.id.0, "stack status refresh timed out");
	}
	let env = env_of(s, &stack)?;
	let files = files_of(s, &stack)?;
	let (warnings, missing_vars) = match compose::parse(
		&stack.compose_yaml,
		&Inputs {
			env: &env,
			files: &files,
		},
	) {
		Ok(c) => (c.warnings, c.missing_vars.into_iter().collect()),
		Err(_) => (Vec::new(), Vec::new()),
	};
	let job = jobs::latest_for_stack(&s.db, stack.id.0).await?;
	Ok(StackDetail {
		env_keys: env.into_keys().collect(),
		services,
		warnings,
		missing_vars,
		job: job.map(|j| (j.id, j.status)),
		stack,
	})
}

/// Asks the agent how each service's container is doing and stores what
/// changed. Best effort: an unreachable agent leaves the rows as they are.
async fn refresh(s: &AppState, stack: &Stack, services: &mut [StackService]) {
	let Ok(Some(server)) = servers::find(&s.db, stack.server_id).await else {
		return;
	};
	let Ok(client) = s.agents.client(s, &server).await else {
		return;
	};
	for svc in services.iter_mut() {
		let Some(container) = svc.container_id.clone() else {
			continue;
		};
		if !matches!(svc.status, ServiceStatus::Running | ServiceStatus::Failed) {
			continue;
		}
		let path = format!("/v1/containers/{container}/health");
		let Ok(report) = client
			.json::<ContainerHealth>(Method::GET, &path, None::<&()>)
			.await
		else {
			continue;
		};
		let (status, message) = observed(&report);
		let health = report.health.map(|h| match h {
			proto::HealthStatus::Starting => "starting",
			proto::HealthStatus::Healthy => "healthy",
			proto::HealthStatus::Unhealthy => "unhealthy",
		});
		if svc.status != status || svc.health.as_deref() != health {
			if let Err(e) =
				stacks::set_service_status(&s.db, svc.id, status, message.as_deref(), health).await
			{
				tracing::warn!(service = svc.id.0, "failed to store service state: {e}");
				continue;
			}
			svc.status = status;
			svc.status_message = message;
			svc.health = health.map(str::to_string);
		}
	}
}

/// What a container's state means for its service row.
fn observed(report: &ContainerHealth) -> (ServiceStatus, Option<String>) {
	match report.state {
		ContainerState::Exited | ContainerState::Dead => (
			ServiceStatus::Failed,
			Some(format!(
				"exited with code {}",
				report.exit_code.unwrap_or(-1)
			)),
		),
		_ if report.gave_up => (
			ServiceStatus::Failed,
			Some("keeps crashing; the agent stopped restarting it".to_string()),
		),
		_ => (ServiceStatus::Running, None),
	}
}

/// Changes the name, the compose file or the variables. The compose file is
/// checked against the resulting variables, so a change that could not
/// deploy is refused. Nothing is redeployed until [`deploy`]. Caller must
/// hold `update stack`.
pub async fn update(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	body: UpdateInput,
) -> AppResult<Stack> {
	authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	let stack = scope::stack(&s.db, team, project, id).await?;
	if stack.status == StackStatus::Deleting {
		return Err(ServerError::Conflict("stack is being deleted".to_string()));
	}
	if let Change::Set(registry) = body.registry {
		registries::service::pick(&s.db, team, registry).await?;
	}

	let name = match &body.name {
		Some(name) => {
			validate_name(name)?;
			name.trim().to_string()
		}
		None => stack.name.clone(),
	};
	let yaml = body
		.compose_yaml
		.unwrap_or_else(|| stack.compose_yaml.clone());
	let mut env = env_of(s, &stack)?;
	for key in &body.env_unset {
		env.remove(key);
	}
	validate_env(&body.env_set)?;
	env.extend(body.env_set);
	validate_env(&env)?;

	let services = stacks::list_services(&s.db, stack.id).await?;
	let files = files_of(s, &stack)?;
	let (compose, _) = check(s, stack.id, &yaml, &env, &files, &domains_of(&services))?;

	let updated = stacks::update(
		&s.db,
		stack.id,
		&name,
		&yaml,
		&stacks::encrypt_env(&s.secrets, &env)?,
		body.registry,
	)
	.await
	.map_err(unique_conflict)?;
	stacks::sync_services(&s.db, stack.id, &images_of(&compose)).await?;
	Ok(updated)
}

async fn start_deploy(s: &AppState, stack: &Stack) -> AppResult<JobId> {
	stacks::set_status(&s.db, stack.id, StackStatus::Deploying, None).await?;
	queue_or_fail(
		s,
		JobKind::StackDeploy,
		stack,
		stack.id.0,
		"failed to queue deploy job",
	)
	.await
}

/// Queues a deploy: services whose translated spec changed are replaced, the
/// others are left running. Caller must hold `update stack`.
pub async fn deploy(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
) -> AppResult<(Stack, JobId)> {
	authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	let mut stack = scope::stack(&s.db, team, project, id).await?;
	if stack.status == StackStatus::Deleting {
		return Err(ServerError::Conflict("stack is being deleted".to_string()));
	}
	let env = env_of(s, &stack)?;
	let services = stacks::list_services(&s.db, stack.id).await?;
	let files = files_of(s, &stack)?;
	check(
		s,
		stack.id,
		&stack.compose_yaml,
		&env,
		&files,
		&domains_of(&services),
	)?;

	let job = start_deploy(s, &stack).await?;
	stack.status = StackStatus::Deploying;
	stack.status_message = None;
	Ok((stack, job))
}

/// Queues the stop of every container of the stack; the agent will not
/// restart them. Caller must hold `update stack`.
pub async fn stop(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
) -> AppResult<(Stack, JobId)> {
	authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	let stack = scope::stack(&s.db, team, project, id).await?;
	if stack.status == StackStatus::Deleting {
		return Err(ServerError::Conflict("stack is being deleted".to_string()));
	}
	let job = stacks::enqueue(s, JobKind::StackStop, &stack, stack.id.0).await?;
	Ok((stack, job))
}

/// Recreates one service from the stack's current configuration, whether or
/// not its spec changed. Caller must hold `update stack`.
pub async fn restart_service(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	service: &str,
) -> AppResult<(StackService, JobId)> {
	authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	let stack = scope::stack(&s.db, team, project, id).await?;
	let svc = service_row(&s.db, &stack, service).await?;
	if stack.status == StackStatus::Deleting {
		return Err(ServerError::Conflict("stack is being deleted".to_string()));
	}
	let job = stacks::enqueue(s, JobKind::StackRestartService, &stack, svc.id.0).await?;
	Ok((svc, job))
}

/// Marks the stack `deleting` and queues the removal of its containers and
/// network; `purge_volumes` also deletes its volumes. The row disappears when
/// the job finishes. Caller must hold `delete stack`.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	purge_volumes: bool,
) -> AppResult<(Stack, JobId)> {
	authorize(&s.db, user, team, Action::Delete, Resource::Stack).await?;
	let mut stack = scope::stack(&s.db, team, project, id).await?;

	stacks::set_status(&s.db, stack.id, StackStatus::Deleting, None).await?;
	stack.status = StackStatus::Deleting;
	stack.status_message = None;
	let kind = if purge_volumes {
		JobKind::StackPurge
	} else {
		JobKind::StackRemove
	};
	let job = queue_or_fail(s, kind, &stack, stack.id.0, "failed to queue remove job").await?;
	Ok((stack, job))
}

async fn service_row(db: &PgPool, stack: &Stack, name: &str) -> AppResult<StackService> {
	stacks::find_service(db, stack.id, name)
		.await?
		.filter(|svc| svc.status != ServiceStatus::Removing)
		.ok_or(ServerError::NotFound)
}

/// Routes `host` to a service of the stack. `port` is the container port;
/// it may be left out when the service exposes exactly one. Takes effect on
/// the next deploy. Caller must hold `update stack`.
#[allow(clippy::too_many_arguments)]
pub async fn set_domain(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	service: &str,
	host: &str,
	port: Option<u16>,
) -> AppResult<StackService> {
	authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	let stack = scope::stack(&s.db, team, project, id).await?;
	let svc = service_row(&s.db, &stack, service).await?;

	let host = host.trim().to_ascii_lowercase();
	if !validate::hostname(&host) {
		return Err(ServerError::Validation("invalid hostname".to_string()));
	}
	if stacks::hostname_in_use(&s.db, &host, svc.id).await? {
		return Err(ServerError::DomainTaken);
	}

	let env = env_of(s, &stack)?;
	let mut domains = domains_of(&stacks::list_services(&s.db, stack.id).await?);
	domains.insert(
		svc.name.clone(),
		Domain {
			host: host.clone(),
			port,
		},
	);
	let files = files_of(s, &stack)?;
	check(s, stack.id, &stack.compose_yaml, &env, &files, &domains)?;

	stacks::set_domain(&s.db, svc.id, Some(&host), port.map(i32::from))
		.await
		.map_err(unique_conflict)?;
	service_row(&s.db, &stack, service).await
}

/// Stops routing a hostname to the service. Takes effect on the next deploy.
/// Caller must hold `update stack`.
pub async fn clear_domain(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	service: &str,
) -> AppResult<StackService> {
	authorize(&s.db, user, team, Action::Update, Resource::Stack).await?;
	let stack = scope::stack(&s.db, team, project, id).await?;
	let svc = service_row(&s.db, &stack, service).await?;
	stacks::set_domain(&s.db, svc.id, None, None).await?;
	service_row(&s.db, &stack, service).await
}

/// One service's container log from its agent. `tail` is capped at 5000
/// lines. Needs `read stack`; the service must have a container.
#[allow(clippy::too_many_arguments)]
pub async fn service_logs(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	service: &str,
	tail: u32,
	follow: bool,
) -> AppResult<BoxStream<'static, Result<proto::Event, ServerError>>> {
	let stack = find(&s.db, user, team, project, id).await?;
	let svc = service_row(&s.db, &stack, service).await?;
	let Some(container_id) = svc.container_id.as_deref() else {
		return Err(ServerError::Validation(
			"service is not running".to_string(),
		));
	};
	let server = scope::server(&s.db, team, stack.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let path = format!(
		"/v1/containers/{container_id}/logs?tail={}&follow={follow}",
		tail.min(MAX_TAIL)
	);
	client.stream(Method::GET, &path, None::<&()>).await
}

/// The state a freshly deployed service row gets.
pub(super) fn deployed_state<'a>(container: &'a str, hash: &'a str) -> ServiceState<'a> {
	ServiceState {
		status: ServiceStatus::Running,
		message: None,
		container_id: Some(container),
		health: None,
		spec_hash: Some(hash),
	}
}
