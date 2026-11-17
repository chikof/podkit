use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use runtime::{ContainerId, ContainerRuntime, LogsQuery, ServerConnection};
use serde::{Deserialize, Serialize};

use super::{Deployment, DeploymentStatus};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DeploymentId, ProjectId, TeamId};
use crate::state::AppState;
use crate::{deploy_pipeline, deployments, scope};

const DEFAULT_PAGE_SIZE: i64 = 50;
const MAX_PAGE_SIZE: i64 = 200;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{deployment_id}", get(get_one))
		.route("/{deployment_id}/rollback", post(rollback))
		.route("/{deployment_id}/logs", get(logs))
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

#[derive(Deserialize)]
struct LogsParams {
	/// `None` returns every buffered line; `Some(n)` returns the last `n`.
	#[serde(default)]
	tail: Option<u32>,
}

/// Triggers a new deployment: queues the row and hands the actual build +
/// run pipeline to a background task, returning immediately. Poll `GET
/// .../deployments/{id}` for status.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
	authorize(&s.db, user.id, team, Action::Create, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	let d = deploy_pipeline::queue_and_spawn(&s, app, Some(user.id)).await?;
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

	let d = deploy_pipeline::queue_rollback_and_spawn(&s, app, &target, Some(user.id)).await?;
	Ok((StatusCode::ACCEPTED, Json(DeploymentResponse::from(&d))))
}

/// Fetches a buffered snapshot of the deployment's container stdout/stderr
/// (`ContainerRuntime::logs`; no live-follow). Returns an empty list before
/// the container exists yet (still `queued`/`building`), rather than
/// erroring, so callers should poll this the same way as `GET
/// .../deployments/{id}`.
async fn logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
	Query(params): Query<LogsParams>,
) -> AppResult<Json<Vec<String>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Deployment).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let d = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;

	let Some(container_id) = d.container_id else {
		return Ok(Json(Vec::new()));
	};

	let server = scope::server(&s.db, team, app.server_id).await?;
	let connection = ServerConnection::connect(&server.connect_target(), &s.secrets)
		.await
		.map_err(|e| {
			tracing::warn!(server = %server.id, "failed to connect to server: {e}");
			ServerError::AgentUnavailable
		})?;

	let lines = connection
		.runtime
		.logs(
			&ContainerId(container_id),
			LogsQuery {
				tail: params.tail,
				..LogsQuery::default()
			},
		)
		.await
		.map_err(|e| {
			tracing::warn!(deployment = %d.id, "failed to read container logs: {e}");
			ServerError::AgentUnavailable
		})?;

	Ok(Json(lines))
}
