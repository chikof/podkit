use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Deployment, DeploymentStatus, service};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DeploymentId, ProjectId, TeamId};
use crate::state::AppState;
use crate::{deployments, logs, scope};

const DEFAULT_PAGE_SIZE: i64 = 50;
const MAX_PAGE_SIZE: i64 = 200;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{deployment_id}", get(get_one))
		.route("/{deployment_id}/rollback", post(rollback))
		.route("/{deployment_id}/cancel", post(cancel))
		.route("/{deployment_id}/build-logs", get(build_logs))
}

#[derive(Serialize)]
pub struct DeploymentResponse {
	pub id: DeploymentId,
	pub application_id: ApplicationId,
	pub status: DeploymentStatus,
	pub commit_sha: Option<String>,
	pub image_tag: Option<String>,
	pub container_id: Option<String>,
	pub error_message: Option<String>,
	/// The deployment of another environment this one was promoted from.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub promoted_from: Option<DeploymentId>,
	/// The name of that deployment's environment.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub promoted_from_environment: Option<String>,
	/// The health gate's verdict: `waiting`, `healthy`, `unhealthy` or `rolled_back`.
	/// Absent when the application has no gate, and in answers that do not look it up.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub health: Option<crate::health_gate::State>,
	/// Why the gate decided what it did, when it says.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub health_detail: Option<String>,
}

impl DeploymentResponse {
	/// Adds what the health gate recorded about each deployment.
	async fn with_health(db: &sqlx::PgPool, mut list: Vec<Self>) -> AppResult<Vec<Self>> {
		let ids: Vec<DeploymentId> = list.iter().map(|d| d.id).collect();
		let mut verdicts = crate::health_gate::verdicts(db, &ids).await?;
		for d in &mut list {
			if let Some((state, detail)) = verdicts.remove(&d.id) {
				d.health = Some(state);
				d.health_detail = detail;
			}
		}
		Ok(list)
	}
}

impl From<&Deployment> for DeploymentResponse {
	fn from(d: &Deployment) -> Self {
		Self {
			id: d.id,
			application_id: d.application_id,
			status: d.status,
			commit_sha: d.commit_sha.clone(),
			image_tag: d.image_tag.clone(),
			container_id: d.container_id.clone(),
			error_message: d.error_message.clone(),
			promoted_from: d.promoted_from,
			promoted_from_environment: d.promoted_from_environment.clone(),
			health: None,
			health_detail: None,
		}
	}
}

#[derive(Deserialize)]
struct ListParams {
	limit: Option<i64>,
	before: Option<DeploymentId>,
}

/// Triggers a new deployment: queues the row and its Deploy job, returning
/// immediately. Poll `GET .../deployments/{id}` for status, or follow
/// `.../build-logs`.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	let d = service::deploy(&s, user.id, team, project, app).await?;
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Lists deployments for an application, newest first, paged with `?limit=`
/// (1..=200, default 50) and `?before=<deployment id>`, each with the health gate's verdict.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<DeploymentResponse>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	let limit = params
		.limit
		.unwrap_or(DEFAULT_PAGE_SIZE)
		.clamp(1, MAX_PAGE_SIZE);
	let rows = deployments::list(&s.db, app.id, params.before, limit).await?;
	let list = rows.iter().map(DeploymentResponse::from).collect();
	Ok(Json(DeploymentResponse::with_health(&s.db, list).await?))
}

/// Fetches a single deployment's current status, with the health gate's verdict. This is
/// the endpoint to poll after `POST .../deployments`.
async fn get_one(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> AppResult<Json<DeploymentResponse>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let d = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;
	let mut one =
		DeploymentResponse::with_health(&s.db, vec![DeploymentResponse::from(&d)]).await?;
	Ok(Json(one.remove(0)))
}

/// Redeploys `{deployment_id}`'s already-built image (no git clone or
/// rebuild) as a new deployment attempt, via the same zero-downtime swap as
/// any other deploy. Rollback is a deploy action, so it needs `create
/// deployment`, not a distinct permission.
async fn rollback(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	let d = service::rollback(&s, user.id, team, project, app, id).await?;
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Cancels a deployment that is still queued, building or deploying. The
/// job worker moves the row to `cancelled` once the agent stream is dropped.
async fn cancel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	let d = service::cancel(&s, user.id, team, project, app, id).await?;
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Streams the deployment's clone/build/start output as server-sent events:
/// everything logged so far, then live lines until the job ends.
async fn build_logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> AppResult<impl IntoResponse> {
	let job = service::build_log_job(&s.db, user.id, team, project, app, id).await?;
	Ok(logs::job_sse(&s, job))
}
