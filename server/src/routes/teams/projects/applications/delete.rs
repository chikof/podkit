use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::ApplicationId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeleteApplicationResponse {
	pub id: i64,
}

/// Deletes an application; caller must hold `delete application` on the
/// team. The application must belong to the project named in the path.
pub async fn delete_application(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id)): Path<(i64, i64, i64)>,
) -> Result<Json<DeleteApplicationResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"delete",
		"application",
	)
	.await?;

	let application = state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.filter(|a| a.project_id.0 == project_id)
		.ok_or(ServerError::ApplicationNotFound)?;

	state.applications.delete(application.id).await?;

	Ok(Json(DeleteApplicationResponse {
		id: application.id.0,
	}))
}
