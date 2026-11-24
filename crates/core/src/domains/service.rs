//! Domain use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database.

use sqlx::PgPool;

use super::{Domain, HOSTNAME_CONSTRAINT};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DomainId, ProjectId, TeamId, UserId};
use crate::{scope, validate};

/// An application's custom domains and what a user needs to point DNS at it.
#[derive(Debug, Clone)]
pub struct DomainList {
	pub domains: Vec<Domain>,
	/// Where a record for the hostname must point: the application's server.
	pub server_host: String,
}

/// Routes a custom hostname to an application; needs `create domain`. The
/// hostname is lower-cased. A hostname already routed anywhere on this
/// install is `DomainTaken`.
pub async fn create(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	hostname: &str,
) -> AppResult<Domain> {
	authorize(db, user, team, Action::Create, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;

	let hostname = hostname.to_ascii_lowercase();
	if !validate::hostname(&hostname) {
		return Err(ServerError::Validation("invalid hostname".to_string()));
	}
	let domain = super::insert(db, app.id, &hostname).await.map_err(|e| {
		if is_unique_violation(&e, HOSTNAME_CONSTRAINT) {
			ServerError::DomainTaken
		} else {
			e.into()
		}
	})?;
	audit::log(
		db,
		user,
		team,
		"domain.create",
		Target::new("domain", domain.id)
			.named(&hostname)
			.in_project(project),
		Diff::new()
			.set("application_id", app.id.0)
			.set("hostname", hostname.clone()),
	)
	.await;
	Ok(domain)
}

/// An application's custom domains, ordered by hostname, with the server's
/// address for the DNS instructions; needs `read domain`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<DomainList> {
	authorize(db, user, team, Action::Read, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	let server = scope::server(db, team, app.server_id).await?;
	Ok(DomainList {
		domains: super::list(db, app.id).await?,
		server_host: server.hostname,
	})
}

/// Unroutes a custom domain of the application; needs `delete domain`.
pub async fn remove(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: DomainId,
) -> AppResult<DomainId> {
	authorize(db, user, team, Action::Delete, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	if !super::delete(db, app.id, id).await? {
		return Err(ServerError::DomainNotFound);
	}
	audit::log(
		db,
		user,
		team,
		"domain.delete",
		Target::new("domain", id).in_project(project),
		Diff::new().set("application_id", app.id.0),
	)
	.await;
	Ok(id)
}
