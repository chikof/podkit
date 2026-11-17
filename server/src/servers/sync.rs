//! Periodic status sync: agent health per server, then crash detection for the
//! deployments the agent is supposed to be running.

use std::collections::HashMap;
use std::time::Duration;

use axum::http::Method;
use futures_util::StreamExt;
use proto::{ContainerInfo, ContainerState};
use tokio_util::sync::CancellationToken;

use super::Server;
use crate::agents::AgentClient;
use crate::error::ServerError;
use crate::ids::{ApplicationId, DeploymentId, ServerId};
use crate::state::AppState;

const INTERVAL: Duration = Duration::from_secs(30);
const CONCURRENT_SERVERS: usize = 8;
const CONTAINER_PREFIX: &str = "podkit-";

struct RunningDeployment {
	id: DeploymentId,
	application_id: ApplicationId,
	container_id: String,
}

/// Checks every active and local server every 30 s until `shutdown`.
pub async fn run(state: AppState, shutdown: CancellationToken) {
	loop {
		sweep(&state).await;
		tokio::select! {
			() = shutdown.cancelled() => return,
			() = tokio::time::sleep(INTERVAL) => {}
		}
	}
}

async fn sweep(state: &AppState) {
	let servers = match super::list_for_sync(&state.db).await {
		Ok(servers) => servers,
		Err(e) => {
			tracing::warn!(error = %e, "status sync could not list servers");
			return;
		}
	};
	futures_util::stream::iter(servers)
		.for_each_concurrent(CONCURRENT_SERVERS, |server| async move {
			check(state, &server).await;
		})
		.await;
}

async fn check(state: &AppState, server: &Server) {
	let client = match state.agents.client(state, server).await {
		Ok(client) => client,
		Err(e) => return mark_unreachable(state, server, &e.to_string()).await,
	};
	if let Err(e) = client.health().await {
		return mark_unreachable(state, server, &e.to_string()).await;
	}
	if let Err(e) = super::mark_active(&state.db, server.id).await {
		tracing::warn!(server = %server.id, error = %e, "status sync could not mark server active");
	}

	if let Err(e) = mark_crashed_deployments(state, server.id, &client).await {
		tracing::warn!(server = %server.id, error = %e, "status sync could not check containers");
	}
}

async fn mark_unreachable(state: &AppState, server: &Server, message: &str) {
	if let Err(e) = super::mark_unreachable(&state.db, server.id, message).await {
		tracing::warn!(server = %server.id, error = %e, "status sync could not mark server unreachable");
	}
}

async fn mark_crashed_deployments(
	state: &AppState,
	server: ServerId,
	client: &AgentClient,
) -> Result<(), ServerError> {
	let running = sqlx::query_as!(
		RunningDeployment,
		r#"
			SELECT d.id AS "id: DeploymentId", d.application_id AS "application_id: ApplicationId",
				d.container_id AS "container_id!"
			FROM deployments d
			JOIN applications a ON a.id = d.application_id
			WHERE a.server_id = $1 AND d.status = 'running' AND d.container_id IS NOT NULL
		"#,
		server as ServerId
	)
	.fetch_all(&state.db)
	.await?;
	if running.is_empty() {
		return Ok(());
	}

	let containers: Vec<ContainerInfo> = client
		.json(
			Method::GET,
			&format!("/v1/containers?prefix={CONTAINER_PREFIX}"),
			None::<&()>,
		)
		.await?;
	let by_id: HashMap<&str, &ContainerInfo> =
		containers.iter().map(|c| (c.id.as_str(), c)).collect();

	for deployment in &running {
		let Some(message) = crash_message(deployment, &by_id) else {
			continue;
		};
		tracing::warn!(deployment = %deployment.id, "{message}");
		sqlx::query!(
			"UPDATE deployments SET status = 'crashed', error_message = $2, finished_at = NOW() WHERE id = $1 AND status = 'running'",
			deployment.id as DeploymentId,
			message
		)
		.execute(&state.db)
		.await?;
	}
	Ok(())
}

/// A running deployment is crashed when its container is gone, or exited/dead
/// with the agent's supervisor having stopped restarting it.
fn crash_message(
	deployment: &RunningDeployment,
	containers: &HashMap<&str, &ContainerInfo>,
) -> Option<String> {
	let Some(container) = containers.get(deployment.container_id.as_str()) else {
		// Agent container names are `podkit-{service}-{instance_id}`.
		return Some(format!(
			"container podkit-app-{}-{} no longer exists",
			deployment.application_id, deployment.id
		));
	};
	let state = match container.state {
		ContainerState::Exited => "exited",
		ContainerState::Dead => "dead",
		_ => return None,
	};
	if !container.gave_up {
		return None;
	}
	let exit_code = container
		.exit_code
		.map_or_else(|| "unknown".to_string(), |code| code.to_string());
	Some(format!(
		"container {} is {state} (exit code {exit_code})",
		container.name
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn deployment() -> RunningDeployment {
		RunningDeployment {
			id: DeploymentId(7),
			application_id: ApplicationId(3),
			container_id: "abc".to_string(),
		}
	}

	fn container(state: ContainerState, exit_code: Option<i64>, gave_up: bool) -> ContainerInfo {
		ContainerInfo {
			id: "abc".to_string(),
			name: "podkit-app-3-7".to_string(),
			state,
			exit_code,
			gave_up,
		}
	}

	fn message(containers: &[ContainerInfo]) -> Option<String> {
		let by_id = containers.iter().map(|c| (c.id.as_str(), c)).collect();
		crash_message(&deployment(), &by_id)
	}

	#[test]
	fn missing_container_is_crashed() {
		assert_eq!(
			message(&[]).as_deref(),
			Some("container podkit-app-3-7 no longer exists")
		);
	}

	#[test]
	fn exited_container_is_crashed_only_once_the_supervisor_gave_up() {
		assert_eq!(
			message(&[container(ContainerState::Exited, Some(137), true)]).as_deref(),
			Some("container podkit-app-3-7 is exited (exit code 137)")
		);
		assert_eq!(
			message(&[container(ContainerState::Dead, None, true)]).as_deref(),
			Some("container podkit-app-3-7 is dead (exit code unknown)")
		);
		assert_eq!(
			message(&[container(ContainerState::Exited, Some(1), false)]),
			None
		);
	}

	#[test]
	fn running_container_is_fine() {
		assert_eq!(
			message(&[container(ContainerState::Running, None, false)]),
			None
		);
	}
}
