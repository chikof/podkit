use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use podkit_core::domain::deployment::Deployment;
use podkit_core::domain::shared::ids::{ApplicationId, DeploymentId, ProjectId, UserId};
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, deploy_pipeline, error::ServerError};

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

/// Redeploys `{deployment_id}`'s already-built image (no git clone or
/// rebuild) as a new deployment attempt, via the same zero-downtime swap
/// as any other deploy. Caller must hold `create deployment` on the
/// team, rollback is a deploy action, not a distinct permission.
pub async fn rollback_deployment(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id, deployment_id)): Path<(i64, i64, i64, i64)>,
) -> Result<(StatusCode, Json<DeploymentResponse>), ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
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

	let target = state
		.deployments
		.find_by_id(DeploymentId(deployment_id))
		.await?
		.filter(|d| d.application_id.0 == application_id)
		.ok_or(ServerError::DeploymentNotFound)?;

	let deployment = deploy_pipeline::queue_rollback_and_spawn(
		state,
		application,
		&target,
		Some(UserId(claims.sub)),
	)
	.await
	.map_err(|e| match e {
		deploy_pipeline::RollbackError::NoImage => ServerError::Validation(e.to_string()),
		deploy_pipeline::RollbackError::Domain(domain_err) => domain_err.into(),
	})?;

	Ok((StatusCode::ACCEPTED, Json(deployment.into())))
}
