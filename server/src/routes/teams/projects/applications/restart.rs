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

/// Brings a stopped (or crash-looped-out) application back up by
/// redeploying its most recent successfully-built image. No git clone or
/// rebuild involved, identical mechanism to
/// `POST .../deployments/{did}/rollback`, just aimed at "whichever image
/// was live most recently" instead of requiring the caller to know a
/// specific deployment id. Caller must hold `create deployment` on the
/// team, same permission rollback uses since this is a deploy action, not
/// a distinct one.
///
/// # Errors
/// Returns [`ServerError::Validation`] if the application has never
/// produced a build to redeploy. In that case there's nothing to restart,
/// the caller should trigger a full deploy instead.
pub async fn restart_application(
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

	// Newest first (per DeploymentRepository::list_by_application), so the
	// first row with a real image_tag is the most recent successful build,
	// regardless of whether it's currently Stopped, Failed (a later
	// attempt failed but an earlier one still has a usable image), or even
	// still Running (restarting a healthy app is a valid "just in case").
	let history = state
		.deployments
		.list_by_application(application.id)
		.await?;
	let target = history
		.into_iter()
		.find(|d| d.image_tag.is_some())
		.ok_or_else(|| {
			ServerError::Validation(
				"no previous successful build to restart from, trigger a deploy instead"
					.to_string(),
			)
		})?;

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
