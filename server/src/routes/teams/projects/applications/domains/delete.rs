use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::CustomDomainId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DeleteDomainResponse {
	pub id: i64,
}

/// Unroutes a custom domain; caller must hold `delete domain` on the team.
/// Must belong to the application named in the path.
pub async fn delete_domain(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, application_id, domain_id)): Path<(i64, i64, i64, i64)>,
) -> Result<Json<DeleteDomainResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"delete",
		"domain",
	)
	.await?;

	let domain = state
		.custom_domains
		.find_by_id(CustomDomainId(domain_id))
		.await?
		.filter(|d| d.application_id.0 == application_id)
		.ok_or(ServerError::DomainNotFound)?;

	state.custom_domains.delete(domain.id).await?;

	Ok(Json(DeleteDomainResponse { id: domain.id.0 }))
}
