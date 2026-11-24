//! Metrics endpoints. Each is `GET .../metrics?range=1h|24h|7d` and answers a
//! JSON array of at most 300 points, oldest first.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use super::service::{self, Extras, Target};
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
	Router::new()
		.route("/", get(application))
		.route("/extra", get(application_extra))
}

/// `/teams/{t}/projects/{p}/databases/{d}/metrics`.
pub fn database_router() -> Router<AppState> {
	Router::new()
		.route("/", get(database))
		.route("/extra", get(database_extra))
}

/// `/teams/{t}/projects/{p}/models/{m}/metrics`.
pub fn model_router() -> Router<AppState> {
	Router::new()
		.route("/", get(model))
		.route("/extra", get(model_extra))
}

/// `/teams/{t}/projects/{p}/stacks/{s}/metrics`.
pub fn stack_router() -> Router<AppState> {
	Router::new()
		.route("/", get(stack))
		.route("/extra", get(stack_extra))
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

/// HTTP, volume and event series and the deploy markers of one application; needs `read
/// application`.
async fn application_extra(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Extras>> {
	let range = query.parse()?;
	let target = Target::Application(project, id);
	Ok(Json(
		service::extras(&s.db, user.id, team, target, range).await?,
	))
}

/// Connection, size, volume and event series of one database; needs `read database`.
async fn database_extra(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Extras>> {
	let range = query.parse()?;
	let target = Target::Database(project, id);
	Ok(Json(
		service::extras(&s.db, user.id, team, target, range).await?,
	))
}

/// Token, request, VRAM and event series of one model; needs `read model`.
async fn model_extra(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Extras>> {
	let range = query.parse()?;
	let target = Target::Model(project, id);
	Ok(Json(
		service::extras(&s.db, user.id, team, target, range).await?,
	))
}

/// HTTP, volume and event series of one stack, all services summed; needs `read stack`.
async fn stack_extra(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(query): Query<RangeQuery>,
) -> AppResult<Json<Extras>> {
	let range = query.parse()?;
	let target = Target::Stack(project, id);
	Ok(Json(
		service::extras(&s.db, user.id, team, target, range).await?,
	))
}
