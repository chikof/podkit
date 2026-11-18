//! Who may read which series. Each fn checks the permission, then resolves the
//! resource through `scope`, so another team's id is a 404.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::{Kind, Range, ResourcePoint, ServerPoint};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::AppResult;
use crate::ids::{
	ApplicationId, DatabaseId, ModelId, ProjectId, ServerId, StackId, TeamId, UserId,
};
use crate::scope;

/// # Errors
/// `Forbidden` without `read server`; `ServerNotFound` for another team's server.
pub async fn server(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ServerId,
	range: Range,
) -> AppResult<Vec<ServerPoint>> {
	authorize(db, user, team, Action::Read, Resource::Server).await?;
	let server = scope::server(db, team, id).await?;
	Ok(super::server_series(db, server.id, range, OffsetDateTime::now_utc()).await?)
}

/// # Errors
/// `Forbidden` without `read application`; `ApplicationNotFound` for another team's.
pub async fn application(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
	range: Range,
) -> AppResult<Vec<ResourcePoint>> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(db, team, project, id).await?;
	resource(db, Kind::Application, app.id.0, range).await
}

/// # Errors
/// `Forbidden` without `read database`; `DatabaseNotFound` for another team's.
pub async fn database(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
	range: Range,
) -> AppResult<Vec<ResourcePoint>> {
	authorize(db, user, team, Action::Read, Resource::Database).await?;
	let database = scope::database(db, team, project, id).await?;
	resource(db, Kind::Database, database.id.0, range).await
}

/// # Errors
/// `Forbidden` without `read model`; `ModelNotFound` for another team's.
pub async fn model(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
	range: Range,
) -> AppResult<Vec<ResourcePoint>> {
	authorize(db, user, team, Action::Read, Resource::Model).await?;
	let model = scope::model(db, team, project, id).await?;
	resource(db, Kind::Model, model.id.0, range).await
}

/// # Errors
/// `Forbidden` without `read stack`; `StackNotFound` for another team's.
pub async fn stack(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
	range: Range,
) -> AppResult<Vec<ResourcePoint>> {
	authorize(db, user, team, Action::Read, Resource::Stack).await?;
	let stack = scope::stack(db, team, project, id).await?;
	resource(db, Kind::Stack, stack.id.0, range).await
}

async fn resource(db: &PgPool, kind: Kind, id: i64, range: Range) -> AppResult<Vec<ResourcePoint>> {
	Ok(super::resource_series(db, kind, id, range, OffsetDateTime::now_utc()).await?)
}
