use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::DeploymentId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeploymentResponse {
	pub id: i64,
	pub application_id: i64,
	pub status: String,
	pub commit_sha: Option<String>,
	pub image_tag: Option<String>,
	pub container_id: Option<String>,
	pub error_message: Option<String>,
}

/// Fetches a single deployment's current status. This is the endpoint to
/// poll after `POST .../deployments`. Caller must hold `read deployment` on
/// the team.
pub async fn get_deployment(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, _application_id, deployment_id)): Path<(i64, i64, i64, i64)>,
) -> Result<Json<DeploymentResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"deployment",
	)
	.await?;

	let d = state
		.deployments
		.find_by_id(DeploymentId(deployment_id))
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;

	Ok(Json(DeploymentResponse {
		id: d.id.0,
		application_id: d.application_id.0,
		status: d.status.as_str().to_string(),
		commit_sha: d.commit_sha,
		image_tag: d.image_tag,
		container_id: d.container_id,
		error_message: d.error_message,
	}))
}
