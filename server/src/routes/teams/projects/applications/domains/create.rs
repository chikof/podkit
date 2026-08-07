use axum::Json;
use axum::extract::{Path, State};
use crypto::generate_id;
use podkit_core::domain::custom_domain::CustomDomain;
use podkit_core::domain::shared::errors::DomainError;
use podkit_core::domain::shared::ids::{ApplicationId, CustomDomainId, ProjectId};
use serde::{Deserialize, Serialize};

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct CreateDomainRequest {
	pub hostname: String,
}

#[derive(Serialize)]
pub struct DomainResponse {
	pub id: i64,
	pub application_id: i64,
	pub hostname: String,
}

impl From<CustomDomain> for DomainResponse {
	fn from(d: CustomDomain) -> Self {
		Self {
			id: d.id.0,
			application_id: d.application_id.0,
			hostname: d.hostname,
		}
	}
}

/// Routes a custom hostname to an application. The user's own DNS must
/// point the hostname at the target server, podkit doesn't verify that
/// (it'll just be self-evident when it doesn't work). If `ACME_EMAIL` is
/// configured, this domain gets an automatic HTTPS router on the next
/// deploy; otherwise it's HTTP-only. Caller must hold `create domain` on
/// the team.
pub async fn create_domain(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, project_id, application_id)): Path<(i64, i64, i64)>,
	Json(body): Json<CreateDomainRequest>,
) -> Result<Json<DomainResponse>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"create",
		"domain",
	)
	.await?;

	state
		.projects
		.find_by_id(ProjectId(project_id))
		.await?
		.filter(|p| p.team_id.0 == team_id)
		.ok_or(ServerError::ProjectNotFound)?;

	state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.filter(|a| a.project_id.0 == project_id)
		.ok_or(ServerError::ApplicationNotFound)?;

	let domain = CustomDomain::new(
		CustomDomainId(generate_id()),
		ApplicationId(application_id),
		body.hostname,
	);

	state
		.custom_domains
		.save(&domain)
		.await
		.map_err(|e| match e {
			DomainError::AlreadyExists => ServerError::DomainTaken,
			e => ServerError::Domain(e),
		})?;

	Ok(Json(domain.into()))
}
