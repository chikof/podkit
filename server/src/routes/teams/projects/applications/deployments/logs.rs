use axum::Json;
use axum::extract::{Path, Query, State};
use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::{ContainerId, LogsQuery};
use podkit_core::domain::shared::ids::{ApplicationId, DeploymentId};
use runtime::ServerConnection;
use serde::Deserialize;

use crate::auth::permission::require_permission;
use crate::{AppState, auth::extractor::AuthUser, error::ServerError};

#[derive(Deserialize)]
pub struct LogsParams {
	/// `None` returns every buffered line; `Some(n)` returns the last `n`.
	#[serde(default)]
	pub tail: Option<u32>,
}

/// Fetches a buffered snapshot of the deployment's container stdout/stderr
/// (`ContainerRuntime::logs`; no live-follow yet, see the `LogsQuery` doc
/// comment). Returns an empty list before the container exists yet (still
/// `queued`/`building`), rather than erroring, so callers should poll this
/// the same way as `GET .../deployments/{id}`.
///
/// Caller must hold `read deployment` on the team.
pub async fn get_deployment_logs(
	State(state): State<AppState>,
	AuthUser(claims): AuthUser,
	Path((team_id, _project_id, application_id, deployment_id)): Path<(i64, i64, i64, i64)>,
	Query(params): Query<LogsParams>,
) -> Result<Json<Vec<String>>, ServerError> {
	require_permission(
		state.authorizer.as_ref(),
		claims.sub,
		team_id,
		"read",
		"deployment",
	)
	.await?;

	let deployment = state
		.deployments
		.find_by_id(DeploymentId(deployment_id))
		.await?
		.filter(|d| d.application_id.0 == application_id)
		.ok_or(ServerError::DeploymentNotFound)?;

	let Some(container_id) = deployment.container_id else {
		return Ok(Json(Vec::new()));
	};

	let application = state
		.applications
		.find_by_id(ApplicationId(application_id))
		.await?
		.ok_or(ServerError::ApplicationNotFound)?;

	let server = state
		.servers
		.find_by_id(application.server_id)
		.await?
		.ok_or(ServerError::ServerNotFound)?;

	let connection = ServerConnection::connect(&server, &state.secrets)
		.await
		.map_err(|e| ServerError::Validation(format!("failed to connect to server: {e}")))?;

	let lines = connection
		.runtime
		.logs(
			&ContainerId(container_id),
			LogsQuery {
				tail: params.tail,
				..LogsQuery::default()
			},
		)
		.await?;

	Ok(Json(lines))
}
