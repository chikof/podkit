//! Volume use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or the agent.

use axum::http::Method;
use sqlx::PgPool;

use super::{MOUNT_PATH_CONSTRAINT, Volume};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId, VolumeId};
use crate::state::AppState;
use crate::{scope, validate};

/// Adds a volume to an application; needs `create volume`. It is only
/// mounted from the next deploy on.
pub async fn create(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	mount_path: &str,
) -> AppResult<Volume> {
	authorize(db, user, team, Action::Create, Resource::Volume).await?;
	let app = scope::application(db, team, project, app).await?;

	if !validate::abs_path(mount_path) {
		return Err(ServerError::Validation(
			"mount_path must be an absolute path other than /".to_string(),
		));
	}
	super::insert(db, app.id, mount_path).await.map_err(|e| {
		if is_unique_violation(&e, MOUNT_PATH_CONSTRAINT) {
			ServerError::Conflict("a volume is already mounted at that path".to_string())
		} else {
			e.into()
		}
	})
}

/// An application's volumes, ordered by mount path; needs `read volume`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<Volume>> {
	authorize(db, user, team, Action::Read, Resource::Volume).await?;
	let app = scope::application(db, team, project, app).await?;
	Ok(super::list(db, app.id).await?)
}

/// Removes the volume and its data from the server, then its row. A volume
/// still mounted by a running container is refused: stop the application
/// first. Needs `delete volume`.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: VolumeId,
) -> AppResult<VolumeId> {
	authorize(&s.db, user, team, Action::Delete, Resource::Volume).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let volume = super::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::VolumeNotFound)?;

	let server = scope::server(&s.db, team, app.server_id).await?;
	s.agents
		.client(s, &server)
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
	Ok(volume.id)
}
