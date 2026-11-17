use std::collections::{HashMap, HashSet};
use std::time::Duration;

use runtime::{ContainerId, ContainerRuntime, ContainerState, ServerConnection};
use tracing::{info, warn};

use crate::applications;
use crate::deploy_pipeline::find_server;
use crate::deployments::{self, Deployment, DeploymentStatus};
use crate::ids::DeploymentId;
use crate::state::AppState;

const SWEEP_INTERVAL: Duration = Duration::from_secs(15);
/// Restart attempts allowed per deployment before giving up and marking it
/// `Stopped` rather than restart forever. A crash-looping image should
/// surface as a problem, not spin invisibly.
const MAX_RESTART_ATTEMPTS: u32 = 5;

/// Spawns podkit's own restart-policy enforcement. Podman is daemonless and
/// does not restart crashed containers on its own: its
/// `HostConfig.RestartPolicy` is honored only at `podman service` boot or
/// via a systemd/Quadlet unit, never as a live background watchdog (we
/// checked by hand: `podman kill` on an `on-failure` container left it
/// `Exited` indefinitely). This is the process that actually delivers on
/// the "restart on crash" promise. Runs for the process lifetime.
pub fn spawn(state: AppState) {
	tokio::spawn(async move {
		let mut attempts: HashMap<DeploymentId, u32> = HashMap::new();
		loop {
			tokio::time::sleep(SWEEP_INTERVAL).await;
			sweep(&state, &mut attempts).await;
		}
	});
}

async fn sweep(state: &AppState, attempts: &mut HashMap<DeploymentId, u32>) {
	let running = match deployments::list_running(&state.db).await {
		Ok(rows) => rows,
		Err(e) => {
			warn!("health monitor: failed to list running deployments: {e}");
			return;
		}
	};

	// Drop counters for deployments no longer `Running` (redeployed over,
	// rolled back, or already given up on) so a stale count never lingers.
	let still_running: HashSet<DeploymentId> = running.iter().map(|d| d.id).collect();
	attempts.retain(|id, _| still_running.contains(id));

	for deployment in &running {
		if let Err(e) = check_and_heal(state, deployment, attempts).await {
			warn!(deployment = deployment.id.0, "health monitor: {e}");
		}
	}
}

async fn check_and_heal(
	state: &AppState,
	deployment: &Deployment,
	attempts: &mut HashMap<DeploymentId, u32>,
) -> Result<(), String> {
	let Some(container_id) = deployment.container_id.clone() else {
		return Ok(()); // shouldn't happen for a Running row, but nothing to check
	};

	let application = applications::find_by_id(&state.db, deployment.application_id)
		.await
		.map_err(|e| e.to_string())?
		.ok_or("application not found")?;

	let server = find_server(&state.db, application.id)
		.await
		.map_err(|e| e.to_string())?
		.ok_or("server not found")?;

	let connection = ServerConnection::connect(&server.connect_target(), &state.secrets)
		.await
		.map_err(|e| e.to_string())?;

	let id = ContainerId(container_id);
	let status = connection
		.runtime
		.inspect(&id)
		.await
		.map_err(|e| e.to_string())?;

	if status.state == ContainerState::Running {
		attempts.remove(&deployment.id); // healthy, forgive past attempts
		return Ok(());
	}

	let count = attempts.entry(deployment.id).or_insert(0);
	*count += 1;

	if *count > MAX_RESTART_ATTEMPTS {
		warn!(
			deployment = deployment.id.0,
			"exceeded {MAX_RESTART_ATTEMPTS} restart attempts, giving up"
		);
		let mut dying = deployment.clone();
		dying.error_message = Some(format!(
			"container crashed and exceeded {MAX_RESTART_ATTEMPTS} restart attempts"
		));
		if dying.transition(DeploymentStatus::Stopped).is_ok() {
			let _ = deployments::update(&state.db, &dying).await;
		}
		attempts.remove(&deployment.id);
		return Ok(());
	}

	info!(
		deployment = deployment.id.0,
		attempt = *count,
		"container exited unexpectedly, restarting"
	);
	connection
		.runtime
		.start_container(&id)
		.await
		.map_err(|e| e.to_string())
}
