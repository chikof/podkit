use std::time::Duration;

use runtime::{
	BuildSpec, ContainerId, ContainerRuntime, ContainerSpec, ContainerState, ImageRef,
	ResourceLimits, RestartPolicy, ServerConnection,
};
use tracing::{error, info, warn};

use crate::applications::{self, Application};
use crate::deployments::{self, Deployment, DeploymentStatus};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ServerId, TeamId, UserId};
use crate::servers::{Server, ServerStatus};
use crate::state::AppState;
use crate::{domains, env_vars};

/// How many `inspect` polls (1s apart) a freshly-started container must
/// survive before we call it healthy and retire the old one. This checks
/// container-state health (still running, hasn't crash-looped), not an
/// app-level HTTP probe: we don't know each app's health endpoint.
const HEALTH_CHECK_ATTEMPTS: u32 = 3;

/// Queues a new deployment row and spawns [`run`] to build from source and
/// deploy, returning the freshly-queued row immediately. Shared by the
/// authenticated trigger route and the webhook receiver, the only
/// difference between them is who's allowed to call this and whether
/// `triggered_by` is set.
///
/// # Errors
/// Returns an error if the row can't be persisted.
pub async fn queue_and_spawn(
	state: &AppState,
	application: Application,
	triggered_by: Option<UserId>,
) -> Result<Deployment, ServerError> {
	let deployment = deployments::insert(&state.db, application.id, triggered_by, None).await?;

	tokio::spawn(run(state.clone(), application, deployment.clone()));

	Ok(deployment)
}

/// Queues a new deployment row that redeploys `target`'s already-built
/// image, skipping git clone and build entirely. Still a *new* row (we
/// never mutate deployment history), and still goes through the same
/// zero-downtime swap as a normal deploy.
///
/// # Errors
/// Returns [`ServerError::Validation`] if `target` never reached a build (no
/// `image_tag`), or an error if the row can't be persisted.
pub async fn queue_rollback_and_spawn(
	state: &AppState,
	application: Application,
	target: &Deployment,
	triggered_by: Option<UserId>,
) -> Result<Deployment, ServerError> {
	let image_tag = target.image_tag.clone().ok_or_else(|| {
		ServerError::Validation("target deployment never produced an image to roll back to".into())
	})?;

	let deployment = deployments::insert(
		&state.db,
		application.id,
		triggered_by,
		target.commit_sha.as_deref(),
	)
	.await?;

	tokio::spawn(run_rollback(
		state.clone(),
		application,
		deployment.clone(),
		image_tag,
	));

	Ok(deployment)
}

/// Runs one deployment attempt end to end: git clone -> build image ->
/// zero-downtime swap into place. Any failure short-circuits to `Failed`
/// with `error_message` set, the currently-running container (if any) is
/// left untouched. Spawned in the background by [`queue_and_spawn`],
/// the HTTP request returns as soon as the row is queued.
async fn run(state: AppState, application: Application, mut deployment: Deployment) {
	let Some(server) = lookup_server(&state, &mut deployment, &application).await else {
		return;
	};

	if !advance(&state, &mut deployment, DeploymentStatus::Building).await {
		return;
	}

	// One credential per application: an ssh key for ssh repo urls, a token
	// for https ones. `create`/`update` only ever store the matching one.
	let credential = match application
		.git_token
		.as_deref()
		.or(application.deploy_key.as_deref())
		.map(|enc| state.secrets.decrypt(enc))
		.transpose()
	{
		Ok(credential) => credential,
		Err(e) => {
			fail(
				&state,
				&mut deployment,
				format!("failed to decrypt repository credential: {e}"),
			)
			.await;
			return;
		}
	};

	let cloned = match runtime::clone_to_tar(
		&application.repo_url,
		&application.git_ref,
		credential.as_deref(),
		application.build_strategy.into(),
		&application.dockerfile_path,
	)
	.await
	{
		Ok(c) => c,
		Err(e) => {
			fail(&state, &mut deployment, format!("git clone failed: {e}")).await;
			return;
		}
	};
	deployment.commit_sha = Some(cloned.commit_sha.clone());

	let connection = match ServerConnection::connect(&server.connect_target(), &state.secrets).await
	{
		Ok(c) => c,
		Err(e) => {
			fail(
				&state,
				&mut deployment,
				format!("failed to connect to server: {e}"),
			)
			.await;
			return;
		}
	};

	let short_sha = &cloned.commit_sha[..cloned.commit_sha.len().min(12)];
	let image_tag = format!("podkit-app-{}:{short_sha}", application.id.0);

	if let Err(e) = connection
		.runtime
		.build_image(BuildSpec {
			tag: ImageRef(image_tag.clone()),
			dockerfile_path: cloned.dockerfile_path,
			context_tar: cloned.context_tar,
		})
		.await
	{
		fail(&state, &mut deployment, format!("image build failed: {e}")).await;
		return;
	}

	deploy_and_swap(
		&state,
		&application,
		&mut deployment,
		&server,
		&connection,
		image_tag,
	)
	.await;
}

/// Redeploys an already-built image with no clone/build phase, used by
/// [`queue_rollback_and_spawn`]. Still goes `queued -> building -> deploying
/// -> running`; `building` is near-instant since there's nothing to
/// build, it's just "preparing to deploy an existing image".
async fn run_rollback(
	state: AppState,
	application: Application,
	mut deployment: Deployment,
	image_tag: String,
) {
	let Some(server) = lookup_server(&state, &mut deployment, &application).await else {
		return;
	};

	if !advance(&state, &mut deployment, DeploymentStatus::Building).await {
		return;
	}

	let connection = match ServerConnection::connect(&server.connect_target(), &state.secrets).await
	{
		Ok(c) => c,
		Err(e) => {
			fail(
				&state,
				&mut deployment,
				format!("failed to connect to server: {e}"),
			)
			.await;
			return;
		}
	};

	deploy_and_swap(
		&state,
		&application,
		&mut deployment,
		&server,
		&connection,
		image_tag,
	)
	.await;
}

async fn lookup_server(
	state: &AppState,
	deployment: &mut Deployment,
	application: &Application,
) -> Option<Server> {
	match find_server(&state.db, application.id).await {
		Ok(Some(server)) => Some(server),
		Ok(None) => {
			fail(state, deployment, "target server not found").await;
			None
		}
		Err(e) => {
			fail(
				state,
				deployment,
				format!("failed to look up target server: {e}"),
			)
			.await;
			None
		}
	}
}

/// The pipeline runs without a team context, so the server is reached
/// through the application that targets it rather than by its own id.
pub(crate) async fn find_server(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
) -> Result<Option<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT s.id AS "id: ServerId", s.team_id AS "team_id: TeamId", s.name, s.hostname,
				s.ssh_port, s.ssh_user, s.ssh_private_key, s.podman_socket_path, s.is_local,
				s.status AS "status: ServerStatus"
			FROM servers s
			JOIN applications a ON a.server_id = s.id
			WHERE a.id = $1
		"#,
		application_id.0
	)
	.fetch_optional(db)
	.await
}

/// Shared tail of both [`run`] and [`run_rollback`]: `Deploying` ->
/// provision ingress -> create + start the new container under a
/// per-deployment name -> health-check it -> only then retire whatever
/// container the application's previous *running* deployment left behind.
/// This ordering is the whole point: the old container is never touched
/// until the new one has proven itself.
async fn deploy_and_swap(
	state: &AppState,
	application: &Application,
	deployment: &mut Deployment,
	server: &Server,
	connection: &ServerConnection,
	image_tag: String,
) {
	deployment.image_tag = Some(image_tag.clone());
	if !advance(state, deployment, DeploymentStatus::Deploying).await {
		return;
	}

	let acme = state
		.config
		.acme_email
		.as_deref()
		.map(|email| runtime::AcmeConfig { email });
	if !provision_ingress(state, deployment, server, connection, acme.as_ref()).await {
		return;
	}

	let Some(container_id) = create_and_start_container(
		state,
		application,
		deployment,
		server,
		connection,
		image_tag,
		acme.is_some(),
	)
	.await
	else {
		return;
	};

	if let Err(e) = health_check(connection, &container_id).await {
		// New container never proved itself, so leave the old one serving
		// and clean up the failed attempt.
		let _ = connection
			.runtime
			.remove_container(&container_id, true)
			.await;
		fail(
			state,
			deployment,
			format!("new container failed health check: {e}"),
		)
		.await;
		return;
	}

	deployment.container_id = Some(container_id.0);
	if !advance(state, deployment, DeploymentStatus::Running).await {
		return;
	}
	info!(
		deployment = deployment.id.0,
		application = application.id.0,
		"deployment running"
	);

	// Give Traefik's docker-provider poll cycle time to add the new
	// container to the pool before we touch the old one. Without this,
	// a request can land on the old container after we've stopped it but
	// before Traefik has routed around it.
	tokio::time::sleep(Duration::from_secs(3)).await;

	retire_previous_container(state, application, deployment, connection).await;
}

/// Makes sure Traefik is up and configured for `server` before the new
/// container goes live. Reports failure via [`fail`] and returns `false` so
/// the caller can bail out the same way as any other step.
async fn provision_ingress(
	state: &AppState,
	deployment: &mut Deployment,
	server: &Server,
	connection: &ServerConnection,
	acme: Option<&runtime::AcmeConfig<'_>>,
) -> bool {
	if let Err(e) = runtime::ensure_traefik(
		&connection.runtime,
		&server.podman_socket_path,
		state.config.ingress_port,
		state.config.https_port,
		acme,
	)
	.await
	{
		fail(
			state,
			deployment,
			format!("failed to provision ingress: {e}"),
		)
		.await;
		return false;
	}
	true
}

/// Builds the routing labels and container spec for this deployment,
/// creates the container, and starts it. Returns `None` on any failure,
/// having already reported it via [`fail`].
async fn create_and_start_container(
	state: &AppState,
	application: &Application,
	deployment: &mut Deployment,
	server: &Server,
	connection: &ServerConnection,
	image_tag: String,
	acme_enabled: bool,
) -> Option<ContainerId> {
	let hostname = applications::public_hostname(&application.slug, server);
	// Unique per deployment: the previous deployment's container keeps
	// serving under its own name until we've confirmed this one is healthy.
	let container_name = format!("podkit-app-{}-{}", application.id.0, deployment.id.0);

	let env = match load_env(state, application).await {
		Ok(env) => env,
		Err(e) => {
			fail(state, deployment, e).await;
			return None;
		}
	};

	let custom_domains = match domains::list(&state.db, application.id).await {
		Ok(rows) => rows.into_iter().map(|d| d.hostname).collect::<Vec<_>>(),
		Err(e) => {
			fail(
				state,
				deployment,
				format!("failed to load custom domains: {e}"),
			)
			.await;
			return None;
		}
	};
	if !custom_domains.is_empty() && !acme_enabled {
		warn!(
			application = application.id.0,
			"application has custom domains but no ACME_EMAIL configured, serving HTTP only, no HTTPS router"
		);
	}

	let mut labels =
		runtime::routing_labels(&application.slug, &hostname, application.container_port);
	if acme_enabled {
		labels.extend(runtime::secure_routing_labels(
			&application.slug,
			&custom_domains,
		));
	}
	let container_id = match connection
		.runtime
		.create_container(ContainerSpec {
			name: container_name,
			image: ImageRef(image_tag),
			command: None,
			env,
			ports: vec![],
			networks: vec![runtime::traefik::NETWORK.to_string()],
			labels,
			binds: vec![],
			resource_limits: ResourceLimits {
				memory_bytes: application
					.memory_limit_mb
					.map(|mb| i64::from(mb) * 1024 * 1024),
				cpu_cores: application.cpu_limit.map(f64::from),
			},
			// Crash-restart by default with bounded retries, a genuinely
			// broken image shouldn't loop forever; manual stop still wins
			// since that's a distinct lifecycle op podkit itself controls
			// (the swap uses stop+remove, not "docker stop" left to restart).
			restart_policy: RestartPolicy::OnFailure {
				max_retries: Some(5),
			},
		})
		.await
	{
		Ok(id) => id,
		Err(e) => {
			fail(state, deployment, format!("container create failed: {e}")).await;
			return None;
		}
	};

	if let Err(e) = connection.runtime.start_container(&container_id).await {
		fail(state, deployment, format!("container start failed: {e}")).await;
		return None;
	}

	Some(container_id)
}

/// Polls the container's state; healthy means it's still `Running` after
/// every check in the window, not just immediately after start (catches
/// fast crash-loops that a single point-in-time check would miss).
async fn health_check(connection: &ServerConnection, id: &ContainerId) -> Result<(), String> {
	for attempt in 1..=HEALTH_CHECK_ATTEMPTS {
		tokio::time::sleep(Duration::from_secs(1)).await;
		match connection.runtime.inspect(id).await {
			Ok(status) if status.state == ContainerState::Running => {}
			Ok(status) => {
				return Err(format!(
					"container state is {:?} on health check {attempt}",
					status.state
				));
			}
			Err(e) => return Err(format!("inspect failed on health check {attempt}: {e}")),
		}
	}
	Ok(())
}

async fn load_env(
	state: &AppState,
	application: &Application,
) -> Result<Vec<(String, String)>, String> {
	let rows = env_vars::list(&state.db, application.id)
		.await
		.map_err(|e| format!("failed to load env vars: {e}"))?;

	let mut env = Vec::with_capacity(rows.len());
	for row in rows {
		let value = state
			.secrets
			.decrypt(&row.value)
			.map_err(|e| format!("failed to decrypt env var {}: {e}", row.key))?;
		env.push((row.key, value));
	}
	Ok(env)
}

/// Finds the application's previous `Running` deployment (excluding the one
/// that just went live), removes the container it left behind, and marks
/// it `Stopped` (`Running -> Stopped` is the one non-linear transition we
/// allow, and this is exactly that case). Marking it matters beyond
/// bookkeeping: [`crate::health_monitor`] only heals deployments still
/// marked `Running`, so a stale `Running` row here would make it "heal" a
/// container we intentionally retired.
async fn retire_previous_container(
	state: &AppState,
	application: &Application,
	current: &Deployment,
	connection: &ServerConnection,
) {
	let mut previous =
		match deployments::previous_running(&state.db, application.id, current.id).await {
			Ok(Some(previous)) => previous,
			Ok(None) => return, // first deploy for this app, nothing to retire
			Err(e) => {
				warn!("failed to look up previous deployment for swap-out: {e}");
				return;
			}
		};

	let Some(container_id) = previous.container_id.clone() else {
		return;
	};

	let id = ContainerId(container_id);
	// Graceful stop first (SIGTERM, grace period) so in-flight requests to
	// the old container finish instead of being dropped by a hard kill.
	// This is the last mile of the zero-downtime guarantee: Traefik
	// already stopped routing new requests here once the new container
	// went healthy, but connections already in progress deserve to finish.
	let _ = connection.runtime.stop_container(&id).await;
	if let Err(e) = connection.runtime.remove_container(&id, true).await {
		warn!(
			deployment = previous.id.0,
			"failed to retire previous container: {e}"
		);
	}

	if let Err(e) = previous.transition(DeploymentStatus::Stopped) {
		warn!(deployment = previous.id.0, "{e}");
		return;
	}
	if let Err(e) = deployments::update(&state.db, &previous).await {
		warn!(
			deployment = previous.id.0,
			"failed to persist retired deployment status: {e}"
		);
	}
}

/// Applies a deployment status transition, persists it, and reports whether
/// it succeeded. Callers should stop the pipeline on `false`; the
/// transition itself already logged why.
async fn advance(state: &AppState, deployment: &mut Deployment, next: DeploymentStatus) -> bool {
	if let Err(e) = deployment.transition(next) {
		error!(deployment = deployment.id.0, "{e}");
		return false;
	}
	if let Err(e) = deployments::update(&state.db, deployment).await {
		error!(
			deployment = deployment.id.0,
			"failed to persist deployment status: {e}"
		);
		return false;
	}
	true
}

async fn fail(state: &AppState, deployment: &mut Deployment, message: impl Into<String>) {
	let message = message.into();
	error!(deployment = deployment.id.0, "{message}");
	deployment.error_message = Some(message);
	let _ = deployment.transition(DeploymentStatus::Failed);
	let _ = deployments::update(&state.db, deployment).await;
}
