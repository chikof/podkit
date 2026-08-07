use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::deployment::{Deployment, DeploymentStatus};
use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::ContainerId;
use podkit_core::domain::shared::ids::{ApplicationId, ProjectId};
use runtime::ServerConnection;
use serde::Serialize;
use tracing::warn;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeploymentResponse {
	pub id: i64,
	pub application_id: i64,
	pub status: String,
	pub commit_sha: Option<String>,
	pub image_tag: Option<String>,
	pub error_message: Option<String>,
}

impl From<Deployment> for DeploymentResponse {
	fn from(d: Deployment) -> Self {
		Self {
			id: d.id.0,
			application_id: d.application_id.0,
			status: d.status.as_str().to_string(),
			commit_sha: d.commit_sha,
			image_tag: d.image_tag,
			error_message: d.error_message,
		}
	}
}

/// Stops the application's currently running deployment: a manual halt,
/// distinct from the automatic `Running -> Stopped` that happens when a
/// redeploy swaps it out (that's the one other place a running deployment
/// gets stopped this way). To bring the app back use
/// `POST .../applications/{aid}/restart`, which redeploys the same image.
///
/// The DB transition is persisted *before* the container is actually
/// stopped, not after: `health_monitor` only "heals" (restarts) deployments
/// still marked `Running`, so flipping the status first is what keeps it
/// from racing in and undoing this on its next 15s sweep. Once persisted,
/// the container stop/remove is best-effort, matching how
/// `deploy_pipeline::retire_previous_container` already treats infra
/// cleanup as secondary to the authoritative DB state.
///
/// Caller must hold `update deployment` on the team.
pub async fn stop_application(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id)): Path<(i64, i64, i64)>,
) -> Result<Json<DeploymentResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"update",
		"deployment",
	)
	.await?;

	state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	let application = state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.filter(|a| a.project_id.0 == project_id)
		.ok_or(ServerError::ApplicationNotFound)?;

	let history = state
		.deployments
		.list_by_application(application.id)
		.await?;
	let mut current = history
		.into_iter()
		.find(|d| d.status == DeploymentStatus::Running)
		.ok_or_else(|| {
			ServerError::Validation("application is not currently running".to_string())
		})?;

	let Some(container_id) = current.container_id.clone() else {
		return Err(ServerError::Validation(
			"running deployment has no container recorded".to_string(),
		));
	};

	current.transition(DeploymentStatus::Stopped)?;
	state.deployments.update(&current).await?;

	let server = state.servers.find_by_id(application.server_id).await?;
	if let Some(server) = server {
		match ServerConnection::connect(&server, &state.secrets).await {
			Ok(connection) => {
				let id = ContainerId(container_id);
				if let Err(e) = connection.runtime.stop_container(&id).await {
					warn!(deployment = current.id.0, "failed to stop container: {e}");
				} else if let Err(e) = connection.runtime.remove_container(&id, true).await {
					warn!(
						deployment = current.id.0,
						"failed to remove stopped container: {e}"
					);
				}
			}
			Err(e) => warn!(
				deployment = current.id.0,
				"failed to connect to server to stop container: {e}"
			),
		}
	} else {
		warn!(
			deployment = current.id.0,
			"target server no longer exists, deployment marked stopped, container left as-is"
		);
	}

	Ok(Json(current.into()))
}
