use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Domain, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, DomainId, ProjectId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{domain_id}", delete(remove))
}

#[derive(Deserialize)]
pub struct CreateDomainRequest {
	pub hostname: String,
}

#[derive(Serialize)]
pub struct DomainResponse {
	pub id: DomainId,
	pub application_id: ApplicationId,
	pub hostname: String,
}

impl From<&Domain> for DomainResponse {
	fn from(d: &Domain) -> Self {
		Self {
			id: d.id,
			application_id: d.application_id,
			hostname: d.hostname.clone(),
		}
	}
}

#[derive(Serialize)]
pub struct DeleteDomainResponse {
	pub id: DomainId,
}

/// Routes a custom hostname to an application. The user's own DNS must
/// point the hostname at the target server, podkit doesn't verify that
/// (it'll just be self-evident when it doesn't work). If `ACME_EMAIL` is
/// configured, this domain gets an automatic HTTPS router on the next
/// deploy; otherwise it's HTTP-only. Caller must hold `create domain` on
/// the team.
pub async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<CreateDomainRequest>,
) -> AppResult<Json<DomainResponse>> {
	let domain = service::create(&s.db, user.id, team, project, app, &body.hostname).await?;
	Ok(Json(DomainResponse::from(&domain)))
}

/// Lists an application's custom domains; caller must hold `read domain`
/// on the team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<DomainResponse>>> {
	let list = service::list(&s.db, user.id, team, project, app).await?;
	Ok(Json(
		list.domains.iter().map(DomainResponse::from).collect(),
	))
}

/// Unroutes a custom domain; caller must hold `delete domain` on the team.
/// Must belong to the application named in the path.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DomainId)>,
) -> AppResult<Json<DeleteDomainResponse>> {
	let id = service::remove(&s.db, user.id, team, project, app, id).await?;
	Ok(Json(DeleteDomainResponse { id }))
}
