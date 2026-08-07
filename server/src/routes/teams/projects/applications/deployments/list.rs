use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::ApplicationId;
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
	pub error_message: Option<String>,
}

/// Lists deployments for an application, most recent first; caller must
/// hold `read deployment` on the team.
pub async fn list_deployments(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, application_id)): Path<(i64, i64, i64)>,
) -> Result<Json<Vec<DeploymentResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"deployment",
	)
	.await?;

	let deployments = state
		.deployments
		.list_by_application(ApplicationId(application_id))
		.await?
		.into_iter()
		.map(|d| DeploymentResponse {
			id: d.id.0,
			application_id: d.application_id.0,
			status: d.status.as_str().to_string(),
			commit_sha: d.commit_sha,
			image_tag: d.image_tag,
			error_message: d.error_message,
		})
		.collect();

	Ok(Json(deployments))
}
