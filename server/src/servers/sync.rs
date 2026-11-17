//! Periodic status sync: agent health per server, then crash detection for the
//! deployments and databases the agent is supposed to be running.

use std::collections::HashMap;
use std::time::Duration;

use axum::http::Method;
use futures_util::StreamExt;
use proto::{ContainerInfo, ContainerState};
use tokio_util::sync::CancellationToken;

use super::Server;
use crate::agents::AgentClient;
use crate::error::ServerError;
use crate::ids::{ApplicationId, DatabaseId, DeploymentId, ServerId};
use crate::state::AppState;

const INTERVAL: Duration = Duration::from_secs(30);
const CONCURRENT_SERVERS: usize = 8;
const CONTAINER_PREFIX: &str = "podkit-";

struct RunningDeployment {
	id: DeploymentId,
	application_id: ApplicationId,
	container_id: String,
}

struct RunningDatabase {
	id: DatabaseId,
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
	// Best effort: a failure here says nothing about reachability.
	super::hardware::refresh(&state.db, server.id, &client).await;

	crate::links::sync(state, server.id, &client).await;

	if let Err(e) = mark_crashed(state, server.id, &client).await {
		tracing::warn!(server = %server.id, error = %e, "status sync could not check containers");
	}
}

async fn mark_unreachable(state: &AppState, server: &Server, message: &str) {
	if let Err(e) = super::mark_unreachable(&state.db, server.id, message).await {
		tracing::warn!(server = %server.id, error = %e, "status sync could not mark server unreachable");
	}
}

/// Marks deployments `crashed` and databases `failed` when the container the
/// row says is running is gone or dead.
async fn mark_crashed(
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
	let databases = sqlx::query_as!(
		RunningDatabase,
		r#"
			SELECT id AS "id: DatabaseId", container_id AS "container_id!"
			FROM databases
			WHERE server_id = $1 AND status = 'running' AND container_id IS NOT NULL
		"#,
		server as ServerId
	)
	.fetch_all(&state.db)
	.await?;
	if running.is_empty() && databases.is_empty() {
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
		let Some(message) = crash_message(&deployment.container_id, &by_id, || {
			// Agent container names are `podkit-{service}-{instance_id}`.
			format!("podkit-app-{}-{}", deployment.application_id, deployment.id)
		}) else {
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

	for database in &databases {
		// The instance id (a job id) is not stored, so a vanished container is named by its service.
		let Some(message) = crash_message(&database.container_id, &by_id, || {
			format!("podkit-db-{}", database.id)
		}) else {
			continue;
		};
		tracing::warn!(database = %database.id, "{message}");
		sqlx::query!(
			"UPDATE databases SET status = 'failed', status_message = $2, updated_at = NOW() WHERE id = $1 AND status = 'running'",
			database.id as DatabaseId,
			message
		)
		.execute(&state.db)
		.await?;
	}
	Ok(())
}

/// A running row is crashed when its container is gone, or exited/dead with
/// the agent's supervisor having stopped restarting it. `missing_name` names
/// the container when it no longer exists.
fn crash_message(
	container_id: &str,
	containers: &HashMap<&str, &ContainerInfo>,
	missing_name: impl FnOnce() -> String,
) -> Option<String> {
	let Some(container) = containers.get(container_id) else {
		return Some(format!("container {} no longer exists", missing_name()));
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
		crash_message("abc", &by_id, || "podkit-app-3-7".to_string())
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
