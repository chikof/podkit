//! The Deploy job: turns one queued deployment row into a running
//! container by streaming `POST /v1/deploy` to the application's server
//! agent and mirroring the agent's events onto the deployment row and the
//! job log.

use axum::http::Method;
use futures_util::StreamExt;
use proto::{
	Build, DeploySpec, Event, GitAuth, Ingress, Phase, RegistryAuth, Routing, Source, Strategy,
	VolumeMount,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{Deployment, DeploymentStatus};
use crate::applications::{self, Application, BuildStrategy, SourceKind, StoredRegistryAuth};
use crate::error::ServerError;
use crate::health_gate::machine::{Machine, Signal};
use crate::ids::{DeploymentId, TeamId};
use crate::jobs::{Job, JobFailure};
use crate::logs::JobLog;
use crate::servers::Server;
use crate::state::AppState;
use crate::{
	deployments, domains, env_vars, links, networks, registries, scope, shared_env, volumes,
};

const LOST_CONNECTION: &str = "lost connection to server agent";

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("deploy job database error: {e}");
	failed("database error")
}

/// Runs one Deploy job. Every `Err` is mapped onto the deployment row by
/// the worker (`jobs::fail_target`), so this only moves the row through
/// `building -> deploying -> running` on the way to success.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`] with a message safe to show the deployment owner.
#[allow(
	clippy::too_many_lines,
	reason = "the deploy stream is one loop over the agent's events"
)]
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let mut deployment = deployments::find_by_id(&state.db, DeploymentId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("deployment not found"))?;
	let application = applications::find_by_id(&state.db, deployment.application_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("application not found"))?;
	let server = load_server(state, &application).await?;

	advance(state, &mut deployment, DeploymentStatus::Building).await?;

	let spec = build_spec(state, &application, &deployment, &server).await?;

	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;
	let mut events = client
		.stream(Method::POST, "/v1/deploy", Some(&spec))
		.await
		.map_err(|e| {
			warn!(server = %server.id, "failed to start deploy stream: {e}");
			failed("server agent unavailable")
		})?;

	let mut deploying = false;
	// Only a deploy with a health gate has a health to follow.
	let mut health = spec
		.health
		.is_some()
		.then(|| Machine::new(0, std::time::Duration::ZERO, false, false));
	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => {
				settle_health(state, &deployment, None).await;
				return Err(JobFailure::Cancelled);
			}
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &line).await,
			Some(Ok(Event::Phase { phase })) => {
				if phase == Phase::HealthCheck {
					follow_health(state, &deployment, &mut health, Signal::Started, None).await;
				}
				if !deploying
					&& matches!(
						phase,
						Phase::Starting | Phase::HealthCheck | Phase::Swapping
					) {
					advance(state, &mut deployment, DeploymentStatus::Deploying).await?;
					deploying = true;
				}
			}
			Some(Ok(Event::Deployed {
				container_id,
				image,
				commit_sha,
			})) => {
				follow_health(state, &deployment, &mut health, Signal::Deployed, None).await;
				if !deploying {
					advance(state, &mut deployment, DeploymentStatus::Deploying).await?;
				}
				deployment.container_id = Some(container_id);
				deployment.image_tag = Some(image);
				if commit_sha.is_some() {
					deployment.commit_sha = commit_sha;
				}
				advance(state, &mut deployment, DeploymentStatus::Running).await?;
				// Queued, never awaited: the deploy is done whether or not the scan ever runs.
				if let Some(image) = deployment.image_tag.as_deref() {
					crate::scanning::queue(
						state,
						application.server_id,
						crate::scanning::Target {
							kind: crate::scanning::ResourceKind::Application,
							resource_id: application.id.0,
							image_ref: image,
							deployment: Some(deployment.id),
						},
					)
					.await;
				}
				if let Err(e) =
					deployments::retire_others(&state.db, application.id, deployment.id).await
				{
					warn!(
						deployment = deployment.id.0,
						"failed to mark replaced deployments stopped: {e}"
					);
				}
				return Ok(());
			}
			Some(Ok(Event::Failed { message })) => {
				follow_health(
					state,
					&deployment,
					&mut health,
					Signal::DeployFailed,
					Some(&message),
				)
				.await;
				settle_health(state, &deployment, Some(&message)).await;
				return Err(JobFailure::Failed(message));
			}
			Some(Ok(
				Event::DumpWritten { .. }
				| Event::Progress { .. }
				| Event::Exited { .. }
				| Event::VolumeDumped { .. }
				| Event::PartsUploaded { .. }
				| Event::Done,
			)) => {}
			Some(Err(e)) => {
				warn!(deployment = deployment.id.0, "deploy stream failed: {e}");
				settle_health(state, &deployment, None).await;
				return Err(failed(LOST_CONNECTION));
			}
			None => {
				settle_health(state, &deployment, None).await;
				return Err(failed(LOST_CONNECTION));
			}
		}
	}
}

/// Feeds the health machine and stores the state it reaches. Best effort: the deploy itself
/// does not depend on this row.
async fn follow_health(
	state: &AppState,
	deployment: &Deployment,
	health: &mut Option<Machine>,
	signal: Signal,
	detail: Option<&str>,
) {
	let Some(machine) = health else {
		return;
	};
	let before = machine.state;
	machine.on(signal, std::time::Duration::ZERO);
	let Some(now) = machine.state.filter(|s| Some(*s) != before) else {
		return;
	};
	if let Err(e) = crate::health_gate::set_state(&state.db, deployment.id, now, detail).await {
		warn!(
			deployment = deployment.id.0,
			"failed to store health state: {e}"
		);
	}
}

/// A deploy that ends without a verdict (cancelled, lost connection) must not stay "waiting
/// for health". Without a detail the row goes; a rollback's row stays, as unhealthy, so the
/// timeline keeps saying what it was.
async fn settle_health(state: &AppState, deployment: &Deployment, detail: Option<&str>) {
	let result = crate::health_gate::settle_waiting(&state.db, deployment.id, detail).await;
	if let Err(e) = result {
		warn!(
			deployment = deployment.id.0,
			"failed to settle health state: {e}"
		);
	}
}

/// The job runs without a team context, so the server is resolved through
/// the application's project.
async fn load_server(state: &AppState, application: &Application) -> Result<Server, JobFailure> {
	let team = sqlx::query_scalar!(
		r#"SELECT team_id AS "team_id!: TeamId" FROM projects WHERE id = $1"#,
		application.project_id.0,
	)
	.fetch_optional(&state.db)
	.await
	.map_err(|e| db_failure(&e))?
	.ok_or_else(|| failed("project not found"))?;

	scope::server(&state.db, team, application.server_id)
		.await
		.map_err(|e| match e {
			ServerError::ServerNotFound => failed("target server not found"),
			ServerError::Sqlx(e) => db_failure(&e),
			_ => failed("failed to look up target server"),
		})
}

#[allow(
	clippy::too_many_lines,
	reason = "the merged cards each added a step; one flow reads better unsplit"
)]
pub(crate) async fn build_spec(
	state: &AppState,
	application: &Application,
	deployment: &Deployment,
	server: &Server,
) -> Result<DeploySpec, JobFailure> {
	let container_port = u16::try_from(application.container_port)
		.map_err(|_| failed("application container_port is out of range"))?;

	let mut source = app_source(state, application, deployment).await?;
	if let (
		Some(id),
		Source::Image {
			auth, pull: true, ..
		},
	) = (application.registry_id, &mut source)
	{
		*auth = Some(
			registries::service::auth_for(state, id)
				.await
				.map_err(|e| {
					tracing::error!(
						application = application.id.0,
						"saved registry unusable: {e}"
					);
					failed("failed to load the saved registry login")
				})?,
		);
	}

	let custom_domains = domains::list(&state.db, application.id)
		.await
		.map_err(|e| {
			tracing::error!("failed to load custom domains: {e}");
			failed("failed to load custom domains")
		})?
		.into_iter()
		.map(|d| d.hostname)
		.collect::<Vec<_>>();
	let https_hosts = if state.config.acme_email.is_some() {
		custom_domains
	} else {
		if !custom_domains.is_empty() {
			warn!(
				application = application.id.0,
				"application has custom domains but no ACME_EMAIL configured, serving HTTP only"
			);
		}
		Vec::new()
	};
	let volumes = volumes::list(&state.db, application.id)
		.await
		.map_err(|e| {
			tracing::error!("failed to load volumes: {e}");
			failed("failed to load volumes")
		})?
		.into_iter()
		.map(|v| VolumeMount {
			name: v.name,
			path: v.mount_path,
			read_only: false,
		})
		.collect::<Vec<_>>();
	// A volume can only be mounted by one container at a time.
	let strategy = if volumes.is_empty() {
		Strategy::Rolling
	} else {
		Strategy::Recreate
	};
	let health = crate::health_gate::config(&state.db, application.id)
		.await
		.map_err(|e| {
			tracing::error!("failed to load health settings: {e}");
			failed("failed to load the health settings")
		})?
		.map(|config| config.to_gate(container_port));

	// An MCP server is reached through the gateway on its environment's network, by its
	// alias: it is never routed publicly, and a stdio one runs behind the agent's bridge.
	let mcp = crate::mcp_servers::find(&state.db, application.id)
		.await
		.map_err(|e| db_failure(&e))?;
	let aliases: Vec<String> = mcp
		.iter()
		.map(|_| crate::mcp_servers::alias(&application.slug))
		.collect();
	let mut options = proto::ContainerOptions::default();
	if let Some(mcp) = mcp
		.as_ref()
		.filter(|m| m.transport == crate::mcp_servers::Transport::Stdio)
	{
		options.mcp_bridge = Some(proto::McpBridge {
			port: container_port,
			path: mcp.path.clone(),
			command: mcp.args(),
		});
	}
	// Internal-only takes the application off the ingress, the way an MCP server is.
	let access = crate::access_rules::for_deploy(&state.db, application.id)
		.await
		.map_err(|e| {
			tracing::error!("failed to load access rules: {e}");
			failed("failed to load the access rules")
		})?;
	let routing = (mcp.is_none() && !access.internal_only).then(|| Routing {
		http_hosts: vec![applications::public_hostname(&application.slug, server)],
		https_hosts,
		access: access.rules,
	});

	Ok(DeploySpec {
		service: format!("app-{}", application.id),
		instance_id: deployment.id.0,
		source,
		env: load_env(state, application).await?,
		command: None,
		container_port: Some(container_port),
		routing,
		network_aliases: Vec::new(),
		volumes,
		published_ports: Vec::new(),
		memory_bytes: application
			.memory_limit_mb
			.map(|mb| i64::from(mb) * 1024 * 1024),
		cpu_cores: application.cpu_limit.map(f64::from),
		strategy,
		gpus: None,
		entrypoint: None,
		shm_bytes: None,
		stop_timeout_s: None,
		networks: Vec::new(),
		options,
		attach: networks::attach::for_resource(
			&state.db,
			networks::attach::Home {
				kind: networks::ResourceKind::Application,
				id: application.id.0,
				environment: application.environment_id,
				server: application.server_id,
			},
			&aliases,
		)
		.await
		.map_err(|e| {
			tracing::error!("failed to resolve networks: {e}");
			failed("failed to resolve the application's networks")
		})?,
		ingress: Ingress {
			http_port: state.config.ingress_port,
			https_port: state.config.https_port,
			acme_email: state.config.acme_email.clone(),
		},
		health,
	})
}

/// What the agent runs. A deployment that already has an image (restart,
/// rollback) reuses it as it is on the server. Otherwise a git app is cloned
/// and built, and an image app pulls its reference again, so a redeploy or a
/// registry webhook picks up a moved tag. After a pull the agent reports the
/// digest reference, which is what a later rollback runs.
pub(crate) async fn app_source(
	state: &AppState,
	application: &Application,
	deployment: &Deployment,
) -> Result<Source, JobFailure> {
	if let Some(reference) = &deployment.image_tag {
		return Ok(Source::Image {
			reference: reference.clone(),
			pull: false,
			auth: None,
		});
	}
	match application.source_kind {
		SourceKind::Image => Ok(Source::Image {
			reference: application
				.image
				.clone()
				.ok_or_else(|| failed("application has no image"))?,
			pull: true,
			auth: registry_auth(state, application)?,
		}),
		SourceKind::Git => Ok(Source::Git {
			repo_url: application
				.repo_url
				.clone()
				.ok_or_else(|| failed("application has no repository"))?,
			git_ref: application.git_ref.clone(),
			auth: git_auth(state, application).await?,
			build: match application.build_strategy {
				BuildStrategy::Dockerfile => Build::Dockerfile {
					path: application.dockerfile_path.clone(),
				},
				BuildStrategy::Nixpacks => Build::Nixpacks,
			},
		}),
	}
}

pub(crate) fn registry_auth(
	state: &AppState,
	application: &Application,
) -> Result<Option<RegistryAuth>, JobFailure> {
	let Some(sealed) = application.registry_auth.as_deref() else {
		return Ok(None);
	};
	let stored = state
		.secrets
		.decrypt(sealed)
		.ok()
		.and_then(|json| serde_json::from_str::<StoredRegistryAuth>(&json).ok())
		.ok_or_else(|| {
			tracing::error!(
				application = application.id.0,
				"registry credential decrypt failed"
			);
			failed("failed to decrypt registry credentials")
		})?;
	Ok(Some(RegistryAuth {
		username: stored.username,
		password: stored.password,
		server: None,
	}))
}

/// One credential per application: an ssh key for ssh repo urls, a token
/// for https ones. `create`/`update` only ever store the matching one.
async fn git_auth(state: &AppState, application: &Application) -> Result<GitAuth, JobFailure> {
	let decrypt = |sealed: &[u8]| {
		state.secrets.decrypt(sealed).map_err(|e| {
			tracing::error!(
				application = application.id.0,
				"credential decrypt failed: {e}"
			);
			failed("failed to decrypt repository credential")
		})
	};
	// A provider application clones with what the provider gives now: a token minted for this
	// clone for a GitHub App, the stored access token for GitLab and Gitea.
	if let (Some(provider), Some(repo)) =
		(application.git_provider_id, application.git_repo.as_deref())
	{
		let access = crate::git_providers::service::clone_access(state, provider, repo)
			.await
			.map_err(|e| {
				tracing::warn!(application = application.id.0, error = %e, "clone access failed");
				failed(format!("could not get access to the repository: {e}"))
			})?;
		return Ok(GitAuth::HttpsToken {
			token: access.token,
			username: access.username,
		});
	}
	if let Some(key) = application.deploy_key_id {
		let sealed = crate::ssh_keys::sealed_private_key(&state.db, key)
			.await
			.map_err(|e| {
				tracing::error!(
					application = application.id.0,
					"deploy key lookup failed: {e}"
				);
				failed("failed to load the deploy key")
			})?
			.ok_or_else(|| failed("the application's deploy key no longer exists"))?;
		let _ = crate::ssh_keys::touch(&state.db, key).await;
		return Ok(GitAuth::SshKey {
			private_key: decrypt(&sealed)?,
		});
	}
	if let Some(sealed) = application.git_token.as_deref() {
		return Ok(GitAuth::HttpsToken {
			token: decrypt(sealed)?,
			username: None,
		});
	}
	Ok(GitAuth::None)
}

pub(crate) async fn load_env(
	state: &AppState,
	application: &Application,
) -> Result<Vec<(String, String)>, JobFailure> {
	let rows = env_vars::list(&state.db, application.id)
		.await
		.map_err(|e| {
			tracing::error!("failed to load env vars: {e}");
			failed("failed to load env vars")
		})?;

	let mut env = Vec::with_capacity(rows.len());
	for row in rows {
		let value = state.secrets.decrypt(&row.value).map_err(|e| {
			tracing::error!(
				application = application.id.0,
				"env var {} decrypt failed: {e}",
				row.key
			);
			failed(format!("failed to decrypt env var {}", row.key))
		})?;
		env.push((row.key, value));
	}
	// Inherit the shared variables (team < project < environment); the application's own win.
	let mut env = shared_env::resolve::app_env(
		state,
		application.project_id,
		application.environment_id,
		env,
	)
	.await
	.map_err(|e| {
		tracing::error!(application = application.id.0, "shared variables: {e}");
		failed(e.to_string())
	})?;
	// The gateway's variables are podkit's to set: they win over a user's of the same name.
	let gateway = links::env(state, application.id).await.map_err(|e| {
		tracing::error!(
			application = application.id.0,
			"failed to load gateway env: {e}"
		);
		failed("failed to load the application's model links")
	})?;
	env.retain(|(key, _)| !gateway.iter().any(|(g, _)| g == key));
	env.extend(gateway);
	let mcp = crate::mcp_servers::env(state, application.id)
		.await
		.map_err(|e| {
			tracing::error!(
				application = application.id.0,
				"failed to load MCP env: {e}"
			);
			failed("failed to load the application's MCP links")
		})?;
	env.retain(|(key, _)| !mcp.iter().any(|(g, _)| g == key));
	env.extend(mcp);
	Ok(env)
}

/// Moves the row to `next` and persists it.
async fn advance(
	state: &AppState,
	deployment: &mut Deployment,
	next: DeploymentStatus,
) -> Result<(), JobFailure> {
	deployment
		.transition(next)
		.map_err(|e| failed(e.to_string()))?;
	deployments::update(&state.db, deployment)
		.await
		.map_err(|e| db_failure(&e))
}
