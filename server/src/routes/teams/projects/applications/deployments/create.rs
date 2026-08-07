use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use podkit_core::domain::deployment::Deployment;
use podkit_core::domain::shared::ids::{ApplicationId, ProjectId, UserId};
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

/// Triggers a new deployment: queues the row and hands the actual build +
/// run pipeline to a background task, returning immediately. Poll `GET
/// .../deployments/{id}` for status.
pub async fn create_deployment(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id)): Path<(i64, i64, i64)>,
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

	let deployment =
		deploy_pipeline::queue_and_spawn(state, application, Some(UserId(claims.sub))).await?;

	Ok((StatusCode::ACCEPTED, Json(deployment.into())))
}
