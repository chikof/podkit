use axum::extract::{Path, State};
use axum::http::Method;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{MOUNT_PATH_CONSTRAINT, Volume};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, VolumeId};
use crate::state::AppState;
use crate::{scope, validate};

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
	authorize(&s.db, user.id, team, Action::Create, Resource::Volume).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	if !validate::abs_path(&body.mount_path) {
		return Err(ServerError::Validation(
			"mount_path must be an absolute path other than /".to_string(),
		));
	}

	let volume = super::insert(&s.db, app.id, &body.mount_path)
		.await
		.map_err(|e| {
			if is_unique_violation(&e, MOUNT_PATH_CONSTRAINT) {
				ServerError::Conflict("a volume is already mounted at that path".to_string())
			} else {
				e.into()
			}
		})?;
	Ok(Json(volume))
}

/// Lists an application's volumes; caller must hold `read volume` on the
/// team.
pub async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<Volume>>> {
	authorize(&s.db, user.id, team, Action::Read, Resource::Volume).await?;
	let app = scope::application(&s.db, team, project, app).await?;

	Ok(Json(super::list(&s.db, app.id).await?))
}

/// Removes the volume and its data from the server, then its row. A volume
/// still mounted by a running container is refused: stop the application
/// first. Caller must hold `delete volume` on the team.
pub async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, VolumeId)>,
) -> AppResult<Json<DeleteVolumeResponse>> {
	authorize(&s.db, user.id, team, Action::Delete, Resource::Volume).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let volume = super::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::VolumeNotFound)?;

	let server = scope::server(&s.db, team, app.server_id).await?;
	s.agents
		.client(&s, &server)
		.await?
		.send(Method::DELETE, &format!("/v1/volumes/{}", volume.name))
		.await
		.map_err(|e| match e {
			ServerError::Conflict(_) => {
				ServerError::Conflict("volume is in use; stop the application first".to_string())
			}
			e => e,
		})?;

	super::delete(&s.db, app.id, volume.id).await?;
	Ok(Json(DeleteVolumeResponse { id: volume.id }))
}
