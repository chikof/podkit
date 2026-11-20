//! Who may read which series. Each fn checks the permission, then resolves the
//! resource through `scope`, so another team's id is a 404.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::events::{DeployMarker, EventMarker};
use super::ext::ExtPoint;
use super::overview::Busiest;
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

/// Which resource an [`extras`] call is about.
#[derive(Debug, Clone, Copy)]
pub enum Target {
	Application(ProjectId, ApplicationId),
	Database(ProjectId, DatabaseId),
	Model(ProjectId, ModelId),
	Stack(ProjectId, StackId),
}

/// Everything beyond CPU and memory that a resource's charts show.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Extras {
	/// HTTP, database, volume, GPU and model series.
	pub points: Vec<ExtPoint>,
	/// Restarts, OOM kills and crashes.
	pub events: Vec<EventMarker>,
	/// Deployments; only applications have them.
	pub deploys: Vec<DeployMarker>,
}

/// The extra series and the markers of one resource over `range`.
///
/// # Errors
/// `Forbidden` without `read` on the resource's kind; the kind's `NotFound` for another team's
/// resource.
pub async fn extras(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	target: Target,
	range: Range,
) -> AppResult<Extras> {
	let now = OffsetDateTime::now_utc();
	let (kind, id) = match target {
		Target::Application(project, id) => {
			authorize(db, user, team, Action::Read, Resource::Application).await?;
			(
				Kind::Application,
				scope::application(db, team, project, id).await?.id.0,
			)
		}
		Target::Database(project, id) => {
			authorize(db, user, team, Action::Read, Resource::Database).await?;
			(
				Kind::Database,
				scope::database(db, team, project, id).await?.id.0,
			)
		}
		Target::Model(project, id) => {
			authorize(db, user, team, Action::Read, Resource::Model).await?;
			(Kind::Model, scope::model(db, team, project, id).await?.id.0)
		}
		Target::Stack(project, id) => {
			authorize(db, user, team, Action::Read, Resource::Stack).await?;
			(Kind::Stack, scope::stack(db, team, project, id).await?.id.0)
		}
	};
	let deploys = if kind == Kind::Application {
		super::events::deploy_markers(db, id, range, now).await?
	} else {
		Vec::new()
	};
	Ok(Extras {
		points: super::ext::series(db, kind, id, range, now).await?,
		events: super::events::markers(db, kind, id, range, now).await?,
		deploys,
	})
}

/// The team's busiest resources, over the kinds the user may read. Needs no permission of its
/// own: a kind the user cannot read is left out.
///
/// # Errors
/// The database rejects the read.
pub async fn busiest(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Busiest> {
	let mut kinds = Vec::new();
	for (kind, resource) in [
		(Kind::Application, Resource::Application),
		(Kind::Database, Resource::Database),
		(Kind::Model, Resource::Model),
		(Kind::Stack, Resource::Stack),
	] {
		if authorize(db, user, team, Action::Read, resource)
			.await
			.is_ok()
		{
			kinds.push(kind);
		}
	}
	let rows = super::overview::load(db, team.0, &kinds).await?;
	Ok(super::overview::rank(&rows))
}
