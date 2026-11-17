use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Deployment, DeploymentStatus};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DeploymentId, ProjectId, TeamId};
use crate::jobs::{self, JobKind};
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
	authorize(&s.db, user.id, team, Action::Create, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	let d = deployments::queue(&s, &app, Some(user.id), None).await?;
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Lists deployments for an application, newest first, paged with `?limit=`
/// (1..=200, default 50) and `?before=<deployment id>`.
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
	Ok(Json(rows.iter().map(DeploymentResponse::from).collect()))
}

/// Fetches a single deployment's current status. This is the endpoint to
/// poll after `POST .../deployments`.
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
	Ok(Json(DeploymentResponse::from(&d)))
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
	authorize(&s.db, user.id, team, Action::Create, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let target = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;

	let d = deployments::queue(&s, &app, Some(user.id), Some(&target)).await?;
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Cancels a deployment that is still queued, building or deploying. The
/// job worker moves the row to `cancelled` once the agent stream is dropped.
async fn cancel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	authorize(&s.db, user.id, team, Action::Update, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let d = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;

	let cancelled = match jobs::latest_for(&s.db, JobKind::Deploy, d.id.0).await? {
		Some(job) => jobs::worker::cancel(&s, job.id).await?,
		None => false,
	};
	if !cancelled {
		return Err(ServerError::Validation(
			"deployment is not in progress".to_string(),
		));
	}

	let d = deployments::find(&s.db, app.id, id).await?.unwrap_or(d);
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Streams the deployment's clone/build/start output as server-sent events:
/// everything logged so far, then live lines until the job ends.
async fn build_logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> AppResult<impl IntoResponse> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let d = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;

	let job = jobs::latest_for(&s.db, JobKind::Deploy, d.id.0).await?;
	Ok(logs::job_sse(&s, job.map(|j| j.id)))
}
