//! HTTP handlers of the links slice. They only translate: the rules live in
//! [`super::service`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Link, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, ModelId, ProjectId, TeamId};
use crate::models::ModelStatus;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list).post(link))
		.route("/{model_id}", axum::routing::delete(unlink))
		.route("/{model_id}/default", put(set_default))
}

type Params = Path<(TeamId, ProjectId, ApplicationId)>;

#[derive(Deserialize)]
struct LinkRequest {
	model_id: ModelId,
	/// Make this the model `PODKIT_MODEL` names. The first link is the default anyway.
	#[serde(default)]
	default: bool,
}

/// No key: it is only ever handed to the application's own deploys.
#[derive(Serialize)]
struct LinkResponse {
	model_id: ModelId,
	name: String,
	slug: String,
	/// What the application sends as `model`.
	served_name: String,
	status: ModelStatus,
	is_default: bool,
}

impl From<Link> for LinkResponse {
	fn from(l: Link) -> Self {
		Self {
			model_id: l.model_id,
			name: l.name,
			slug: l.slug,
			served_name: l.served_name,
			status: l.status,
			is_default: l.is_default,
		}
	}
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Params,
) -> AppResult<Json<Vec<LinkResponse>>> {
	let links = service::list(&s, user.id, team, project, app).await?;
	Ok(Json(links.into_iter().map(Into::into).collect()))
}

async fn link(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Params,
	Json(body): Json<LinkRequest>,
) -> AppResult<(StatusCode, Json<LinkResponse>)> {
	let link = service::link(&s, user.id, team, project, app, body.model_id, body.default).await?;
	Ok((StatusCode::CREATED, Json(link.into())))
}

async fn set_default(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, model)): Path<(TeamId, ProjectId, ApplicationId, ModelId)>,
) -> AppResult<Json<LinkResponse>> {
	let link = service::set_default(&s, user.id, team, project, app, model).await?;
	Ok(Json(link.into()))
}

async fn unlink(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, model)): Path<(TeamId, ProjectId, ApplicationId, ModelId)>,
) -> AppResult<StatusCode> {
	service::unlink(&s, user.id, team, project, app, model).await?;
	Ok(StatusCode::NO_CONTENT)
}
