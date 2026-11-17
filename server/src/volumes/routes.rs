use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Volume, service};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, ProjectId, TeamId, VolumeId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{volume_id}", delete(remove))
}

#[derive(Deserialize)]
pub struct CreateVolumeRequest {
	pub mount_path: String,
}

#[derive(Serialize)]
pub struct DeleteVolumeResponse {
	pub id: VolumeId,
}

/// Adds a volume to an application. It is only mounted from the next
/// deploy on; the running container is untouched. Caller must hold
/// `create volume` on the team.
pub async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<CreateVolumeRequest>,
) -> AppResult<Json<Volume>> {
	let volume = service::create(&s.db, user.id, team, project, app, &body.mount_path).await?;
	Ok(Json(volume))
}

/// Lists an application's volumes; caller must hold `read volume` on the
/// team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<Volume>>> {
	Ok(Json(
		service::list(&s.db, user.id, team, project, app).await?,
	))
}

/// Removes the volume and its data from the server, then its row. A volume
/// still mounted by a running container is refused: stop the application
/// first. Caller must hold `delete volume` on the team.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, VolumeId)>,
) -> AppResult<Json<DeleteVolumeResponse>> {
	let id = service::remove(&s, user.id, team, project, app, id).await?;
	Ok(Json(DeleteVolumeResponse { id }))
}
