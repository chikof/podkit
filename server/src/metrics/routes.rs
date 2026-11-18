//! Metrics endpoints. Each is `GET .../metrics?range=1h|24h|7d` and answers a
//! JSON array of at most 300 points, oldest first.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use super::service;
use super::{Range, ResourcePoint, ServerPoint};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DatabaseId, ModelId, ProjectId, ServerId, StackId, TeamId};
use crate::state::AppState;

/// `/teams/{t}/servers/{s}/metrics`.
pub fn router() -> Router<AppState> {
	Router::new().route("/", get(server))
}

/// `/teams/{t}/projects/{p}/applications/{a}/metrics`.
pub fn application_router() -> Router<AppState> {
	Router::new().route("/", get(application))
}

/// `/teams/{t}/projects/{p}/databases/{d}/metrics`.
pub fn database_router() -> Router<AppState> {
	Router::new().route("/", get(database))
}

/// `/teams/{t}/projects/{p}/models/{m}/metrics`.
pub fn model_router() -> Router<AppState> {
	Router::new().route("/", get(model))
}

/// `/teams/{t}/projects/{p}/stacks/{s}/metrics`.
pub fn stack_router() -> Router<AppState> {
	Router::new().route("/", get(stack))
}

#[derive(Deserialize)]
struct RangeQuery {
	/// `1h` (the default), `24h` or `7d`.
	range: Option<String>,
}

impl RangeQuery {
	fn parse(&self) -> AppResult<Range> {
		match self.range.as_deref() {
			None => Ok(Range::Hour),
			Some(s) => Range::parse(s)
				.ok_or_else(|| ServerError::Validation("range must be 1h, 24h or 7d".to_string())),
		}
	}
}

/// CPU, memory, disk, network and GPU of one server; needs `read server`.
async fn server(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, ServerId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Vec<ServerPoint>>> {
	let range = query.parse()?;
	Ok(Json(
		service::server(&s.db, user.id, team, id, range).await?,
	))
}

/// CPU and memory of one application; needs `read application`.
async fn application(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Vec<ResourcePoint>>> {
	let range = query.parse()?;
	Ok(Json(
		service::application(&s.db, user.id, team, project, id, range).await?,
	))
}

/// CPU and memory of one database; needs `read database`.
async fn database(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Vec<ResourcePoint>>> {
	let range = query.parse()?;
	Ok(Json(
		service::database(&s.db, user.id, team, project, id, range).await?,
	))
}

/// CPU and memory of one model; needs `read model`.
async fn model(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Vec<ResourcePoint>>> {
	let range = query.parse()?;
	Ok(Json(
		service::model(&s.db, user.id, team, project, id, range).await?,
	))
}

/// CPU and memory of one stack, all services summed; needs `read stack`.
async fn stack(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Vec<ResourcePoint>>> {
	let range = query.parse()?;
	Ok(Json(
		service::stack(&s.db, user.id, team, project, id, range).await?,
	))
}
