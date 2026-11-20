//! Stack jobs: deploy (translate and replace what changed), stop, restart one
//! service, remove. Every `Err` is mapped onto the stack row by the worker
//! (`jobs::fail_target`), so success paths only move the rows forward.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::http::Method;
use futures_util::StreamExt;
use proto::{ContainerHealth, Event, HealthStatus, Stream};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::compose::{self, Condition, Inputs};
use super::service::deployed_state;
use super::translate::{self, Domain, PlannedService, Settings};
use super::{
	ServiceState, ServiceStatus, Stack, StackService, StackStatus, find_by_id, find_service_by_id,
};
use crate::agents::AgentClient;
use crate::error::ServerError;
use crate::ids::{StackId, StackServiceId};
use crate::jobs::{Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::networks;
use crate::registries;
use crate::servers;
use crate::shared_env;
use crate::stacks;
use crate::state::AppState;

const LOST_CONNECTION: &str = "lost connection to server agent";
/// How long a service waits for a dependency's health check to pass.
const HEALTH_WAIT: Duration = if cfg!(test) {
	Duration::from_secs(5)
} else {
	Duration::from_mins(5)
};
const HEALTH_POLL: Duration = if cfg!(test) {
	Duration::from_millis(20)
} else {
	Duration::from_secs(1)
};

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("stack job database error: {e}");
	failed("database error")
}

/// Keeps the agent's own client-facing messages and hides the rest.
fn agent_failure(e: ServerError) -> JobFailure {
	match e {
		ServerError::Conflict(m) | ServerError::Validation(m) => failed(m),
		e => {
			warn!("agent request failed: {e}");
			failed("server agent unavailable")
		}
	}
}

/// Runs one Stack* job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`] with a message safe to show the stack's owner.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let (stack, service) = if job.kind == JobKind::StackRestartService {
		let service = find_service_by_id(&state.db, StackServiceId(job.target_id))
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("service not found"))?;
		let stack = find_by_id(&state.db, service.stack_id)
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("stack not found"))?;
		(stack, Some(service))
	} else {
		let stack = find_by_id(&state.db, StackId(job.target_id))
			.await
			.map_err(|e| db_failure(&e))?
			.ok_or_else(|| failed("stack not found"))?;
		(stack, None)
	};
	let removing = matches!(job.kind, JobKind::StackRemove | JobKind::StackPurge);
	if stack.status == StackStatus::Deleting && !removing {
		return Err(failed("stack is being deleted"));
	}

	let server = servers::find(&state.db, stack.server_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("target server not found"))?;
	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;

	let ctx = Ctx {
		state,
		job,
		cancel,
		client: &client,
		stack: &stack,
	};
	match (job.kind, service) {
		(JobKind::StackDeploy, _) => ctx.deploy(log).await,
		(JobKind::StackStop, _) => ctx.stop(log).await,
		(JobKind::StackRemove, _) => ctx.remove(log, false).await,
		(JobKind::StackPurge, _) => ctx.remove(log, true).await,
		(JobKind::StackRestartService, Some(service)) => ctx.restart(log, &service).await,
		_ => Err(failed("not a stack job")),
	}
}

struct Ctx<'a> {
	state: &'a AppState,
	job: &'a Job,
	cancel: &'a CancellationToken,
	client: &'a AgentClient,
	stack: &'a Stack,
}

impl Ctx<'_> {
	/// Parses and translates the stack's current compose file and variables.
	async fn plan(&self, services: &[StackService]) -> Result<translate::Plan, JobFailure> {
		let env = stacks::decrypt_env(&self.state.secrets, self.stack).map_err(|e| {
			tracing::error!(stack = self.stack.id.0, "{e}");
			failed("failed to read the stack's variables")
		})?;
		// The shared variables sit under the stack's own, so `${KEY}` finds them.
		let env = shared_env::resolve::stack_inputs(
			self.state,
			self.stack.project_id,
			self.stack.environment_id,
			env,
		)
		.await
		.map_err(|e| {
			tracing::error!(stack = self.stack.id.0, "shared variables: {e}");
			failed(e.to_string())
		})?;
		let files = stacks::decrypt_files(&self.state.secrets, self.stack).map_err(|e| {
			tracing::error!(stack = self.stack.id.0, "{e}");
			failed("failed to read the stack's files")
		})?;
		let compose = compose::parse(
			&self.stack.compose_yaml,
			&Inputs {
				env: &env,
				files: &files,
			},
		)
		.map_err(|issues| failed(describe(&issues)))?;
		let domains: BTreeMap<String, Domain> = services
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
			.collect();
		let mut plan = translate::plan(
			&compose,
			&Settings {
				stack: self.stack.id.0,
				ingress: proto::Ingress {
					http_port: self.state.config.ingress_port,
					https_port: self.state.config.https_port,
					acme_email: self.state.config.acme_email.clone(),
				},
				domains: &domains,
			},
		)
		.map_err(|issues| failed(describe(&issues)))?;
		// The stack's own network, plus the ones it was attached to.
		let linked = networks::attach::stack_links(&self.state.db, self.stack.id)
			.await
			.map_err(|e| db_failure(&e))?;
		translate::link_networks(&mut plan, &linked);
		Ok(plan)
	}

	async fn services(&self) -> Result<Vec<StackService>, JobFailure> {
		stacks::list_services(&self.state.db, self.stack.id)
			.await
			.map_err(|e| db_failure(&e))
	}

	async fn deploy(&self, log: &mut JobLog) -> Result<(), JobFailure> {
		stacks::set_status(&self.state.db, self.stack.id, StackStatus::Deploying, None)
			.await
			.map_err(|e| db_failure(&e))?;
		let rows = self.services().await?;
		let plan = self.plan(&rows).await?;

		// Services dropped from the compose file go first.
		for row in rows.iter().filter(|r| r.status == ServiceStatus::Removing) {
			log.push(Stream::System, &format!("removing service {}", row.name))
				.await;
			self.client
				.send(
					Method::DELETE,
					&format!(
						"/v1/services/{}",
						translate::service_name(self.stack.id.0, &row.name)
					),
				)
				.await
				.map_err(agent_failure)?;
			stacks::delete_service(&self.state.db, row.id)
				.await
				.map_err(|e| db_failure(&e))?;
		}

		let rows = self.services().await?;
		let mut deployed: BTreeMap<String, StackService> = BTreeMap::new();
		for planned in &plan.services {
			let row = rows
				.iter()
				.find(|r| r.name == planned.name)
				.ok_or_else(|| failed(format!("service {} has no row", planned.name)))?;
			if row.spec_hash.as_deref() == Some(planned.hash.as_str())
				&& row.status == ServiceStatus::Running
				&& row.container_id.is_some()
			{
				log.push(
					Stream::System,
					&format!("{}: unchanged, leaving it running", planned.name),
				)
				.await;
				deployed.insert(planned.name.clone(), row.clone());
				continue;
			}
			self.wait_for_dependencies(log, planned, &deployed).await?;
			let row = self.deploy_service(log, planned, row).await?;
			deployed.insert(planned.name.clone(), row);
		}

		stacks::set_status(&self.state.db, self.stack.id, StackStatus::Running, None)
			.await
			.map_err(|e| db_failure(&e))
	}

	/// Deploys one service and stores the result on its row.
	#[allow(
		clippy::too_many_lines,
		reason = "the merged cards each added a step; one flow reads better unsplit"
	)]
	async fn deploy_service(
		&self,
		log: &mut JobLog,
		planned: &PlannedService,
		row: &StackService,
	) -> Result<StackService, JobFailure> {
		log.push(Stream::System, &format!("{}: deploying", planned.name))
			.await;
		let mut spec = planned.spec.clone();
		spec.instance_id = self.job.id.0;
		// The login is added after the plan is hashed: rotating a password must not
		// redeploy services that run fine.
		if let (
			Some(id),
			proto::Source::Image {
				auth, reference, ..
			},
		) = (self.stack.registry_id, &mut spec.source)
		{
			let login = registries::service::auth_for(self.state, id)
				.await
				.map_err(|e| {
					warn!(stack = self.stack.id.0, "saved registry unusable: {e}");
					failed("failed to load the saved registry login")
				})?;
			// Only the registry it belongs to ever sees the login: a stack's services may
			// come from several.
			if login.server.as_deref() == Some(registries::service::image_host(reference).as_str())
			{
				*auth = Some(login);
			}
		}
		let mut events = self
			.client
			.stream(Method::POST, "/v1/deploy", Some(&spec))
			.await
			.map_err(|e| {
				warn!(
					stack = self.stack.id.0,
					"failed to start service deploy stream: {e}"
				);
				failed("server agent unavailable")
			})?;

		let outcome = loop {
			let event = tokio::select! {
				() = self.cancel.cancelled() => return Err(JobFailure::Cancelled),
				event = events.next() => event,
			};
			match event {
				Some(Ok(Event::Log { stream, line })) => {
					log.push(stream, &format!("{}: {line}", planned.name)).await;
				}
				Some(Ok(Event::Deployed {
					container_id,
					image,
					..
				})) => break Ok((container_id, image)),
				Some(Ok(Event::Failed { message })) => break Err(message),
				Some(Ok(
					Event::Phase { .. }
					| Event::Progress { .. }
					| Event::DumpWritten { .. }
					| Event::Done,
				)) => {}
				Some(Err(e)) => {
					warn!(stack = self.stack.id.0, "service deploy stream failed: {e}");
					break Err(LOST_CONNECTION.to_string());
				}
				None => break Err(LOST_CONNECTION.to_string()),
			}
		};
		match outcome {
			Ok((container_id, image)) => {
				stacks::set_service_state(
					&self.state.db,
					row.id,
					&deployed_state(&container_id, &planned.hash),
				)
				.await
				.map_err(|e| db_failure(&e))?;
				crate::scanning::queue(
					self.state,
					self.stack.server_id,
					crate::scanning::Target {
						kind: crate::scanning::ResourceKind::StackService,
						resource_id: row.id.0,
						image_ref: &image,
						deployment: None,
					},
				)
				.await;
				let mut row = row.clone();
				row.container_id = Some(container_id);
				row.status = ServiceStatus::Running;
				Ok(row)
			}
			Err(message) => {
				let note = format!("{}: {message}", planned.name);
				stacks::set_service_state(
					&self.state.db,
					row.id,
					&ServiceState {
						status: ServiceStatus::Failed,
						message: Some(&message),
						container_id: None,
						health: None,
						spec_hash: None,
					},
				)
				.await
				.map_err(|e| db_failure(&e))?;
				Err(failed(note))
			}
		}
	}

	/// Blocks until every `service_healthy` dependency passes its health
	/// check. A dependency without one cannot ever pass, so that fails at once.
	async fn wait_for_dependencies(
		&self,
		log: &mut JobLog,
		planned: &PlannedService,
		deployed: &BTreeMap<String, StackService>,
	) -> Result<(), JobFailure> {
		for (dep, condition) in &planned.depends_on {
			if *condition != Condition::Healthy {
				continue;
			}
			let container = deployed
				.get(dep)
				.and_then(|row| row.container_id.clone())
				.ok_or_else(|| {
					failed(format!("{}: dependency {dep} did not start", planned.name))
				})?;
			log.push(
				Stream::System,
				&format!("{}: waiting for {dep} to be healthy", planned.name),
			)
			.await;
			let started = Instant::now();
			loop {
				let report = self.health(&container).await?;
				match report.health {
					Some(HealthStatus::Healthy) => break,
					Some(HealthStatus::Unhealthy) => {
						return Err(failed(format!(
							"{}: dependency {dep} is unhealthy",
							planned.name
						)));
					}
					None => {
						return Err(failed(format!(
							"{}: dependency {dep} has no health check to wait for; add a healthcheck to it",
							planned.name
						)));
					}
					Some(HealthStatus::Starting) => {}
				}
				if report.state != proto::ContainerState::Running {
					return Err(failed(format!(
						"{}: dependency {dep} stopped while waiting for it",
						planned.name
					)));
				}
				if started.elapsed() > HEALTH_WAIT {
					return Err(failed(format!(
						"{}: dependency {dep} was not healthy after {} seconds",
						planned.name,
						HEALTH_WAIT.as_secs()
					)));
				}
				tokio::select! {
					() = self.cancel.cancelled() => return Err(JobFailure::Cancelled),
					() = tokio::time::sleep(HEALTH_POLL) => {}
				}
			}
		}
		Ok(())
	}

	async fn health(&self, container: &str) -> Result<ContainerHealth, JobFailure> {
		self.client
			.json(
				Method::GET,
				&format!("/v1/containers/{container}/health"),
				None::<&()>,
			)
			.await
			.map_err(agent_failure)
	}

	/// Stops every container, last service first.
	async fn stop(&self, log: &mut JobLog) -> Result<(), JobFailure> {
		let mut rows = self.services().await?;
		let order = self.stop_order(&rows);
		rows.sort_by_key(|r| {
			order
				.iter()
				.position(|n| *n == r.name)
				.unwrap_or(usize::MAX)
		});
		for row in rows.iter().filter(|r| r.status != ServiceStatus::Removing) {
			let Some(container) = row.container_id.as_deref() else {
				continue;
			};
			log.push(Stream::System, &format!("{}: stopping", row.name))
				.await;
			self.client
				.send(Method::POST, &format!("/v1/containers/{container}/stop"))
				.await
				.map_err(agent_failure)?;
			stacks::set_service_status(&self.state.db, row.id, ServiceStatus::Stopped, None, None)
				.await
				.map_err(|e| db_failure(&e))?;
		}
		stacks::set_status(&self.state.db, self.stack.id, StackStatus::Stopped, None)
			.await
			.map_err(|e| db_failure(&e))
	}

	/// Service names with the ones that depend on others first; the compose
	/// file's start order reversed. Falls back to name order when the file
	/// no longer parses.
	fn stop_order(&self, rows: &[StackService]) -> Vec<String> {
		let env = stacks::decrypt_env(&self.state.secrets, self.stack).unwrap_or_default();
		let files = stacks::decrypt_files(&self.state.secrets, self.stack).unwrap_or_default();
		match compose::parse(
			&self.stack.compose_yaml,
			&Inputs {
				env: &env,
				files: &files,
			},
		) {
			Ok(compose) => {
				let mut order = compose.order();
				order.reverse();
				order
			}
			Err(_) => rows.iter().map(|r| r.name.clone()).collect(),
		}
	}

	/// Recreates one service from the stack's current configuration.
	async fn restart(&self, log: &mut JobLog, service: &StackService) -> Result<(), JobFailure> {
		let rows = self.services().await?;
		let plan = self.plan(&rows).await?;
		let planned = plan
			.services
			.iter()
			.find(|p| p.name == service.name)
			.ok_or_else(|| failed(format!("{} is not in the compose file", service.name)))?;
		self.deploy_service(log, planned, service).await?;
		Ok(())
	}

	/// Removes the containers, the network and, when `purge`, the volumes,
	/// then the rows.
	async fn remove(&self, log: &mut JobLog, purge: bool) -> Result<(), JobFailure> {
		let rows = self.services().await?;
		let network = translate::network_name(self.stack.id.0);
		// The volumes of the compose file as it is now. A volume dropped from
		// the file earlier is not known any more and stays.
		let volumes = if purge {
			self.plan(&rows)
				.await
				.map(|p| p.volumes)
				.unwrap_or_default()
		} else {
			Vec::new()
		};

		log.push(Stream::System, "removing stack containers").await;
		for row in &rows {
			self.client
				.send(
					Method::DELETE,
					&format!(
						"/v1/services/{}",
						translate::service_name(self.stack.id.0, &row.name)
					),
				)
				.await
				.map_err(agent_failure)?;
		}
		log.push(Stream::System, "removing stack network").await;
		self.client
			.send(Method::DELETE, &format!("/v1/networks/{network}"))
			.await
			.map_err(agent_failure)?;
		for volume in &volumes {
			log.push(Stream::System, &format!("removing volume {volume}"))
				.await;
			self.client
				.send(Method::DELETE, &format!("/v1/volumes/{volume}"))
				.await
				.map_err(agent_failure)?;
		}
		stacks::delete(&self.state.db, self.stack.id)
			.await
			.map_err(|e| db_failure(&e))
	}
}

/// The issues of a refused file as one line for the job log and status.
fn describe(issues: &[compose::Issue]) -> String {
	let list = issues
		.iter()
		.take(5)
		.map(|i| {
			if i.path.is_empty() {
				i.message.clone()
			} else {
				format!("{}: {}", i.path, i.message)
			}
		})
		.collect::<Vec<_>>()
		.join("; ");
	let more = issues.len().saturating_sub(5);
	if more > 0 {
		format!("compose file refused: {list}; and {more} more")
	} else {
		format!("compose file refused: {list}")
	}
}
