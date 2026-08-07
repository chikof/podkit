use axum::Json;
use axum::extract::{Path, State};
use podkit_core::domain::shared::ids::ApplicationId;
use serde::Serialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Serialize)]
pub struct DomainResponse {
	pub id: i64,
	pub application_id: i64,
	pub hostname: String,
}

/// Lists an application's custom domains; caller must hold `read domain`
/// on the team.
pub async fn list_domains(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, application_id)): Path<(i64, i64, i64)>,
) -> Result<Json<Vec<DomainResponse>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"domain",
	)
	.await?;

	let domains = state
		.custom_domains
		.list_by_application(ApplicationId(application_id))
		.await?
		.into_iter()
		.map(|d| DomainResponse {
			id: d.id.0,
			application_id: d.application_id.0,
			hostname: d.hostname,
		})
		.collect();

	Ok(Json(domains))
}
