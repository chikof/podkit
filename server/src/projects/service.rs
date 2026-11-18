//! Project use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database.

use sqlx::PgPool;

use super::Project;
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ProjectId, TeamId, UserId};
use crate::scope;
use crate::validate;

/// Constraint violated when a team reuses a project slug.
const SLUG_UNIQUE: &str = "projects_team_id_slug_key";

const MAX_NAME_LEN: usize = 60;

/// A project of the team's list.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
	pub project: Project,
}

/// One project. The resources it holds are `overview::environment_view`.
#[derive(Debug, Clone)]
pub struct ProjectDetail {
	pub project: Project,
}

/// A project name trimmed, 1 to 60 characters.
pub(super) fn validate_name(name: &str) -> AppResult<String> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	Ok(name.to_owned())
}

/// Creates a project; the caller must hold `create project` on the team.
pub async fn create(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	name: &str,
	slug: &str,
) -> AppResult<Project> {
	authorize(db, user, team, Action::Create, Resource::Project).await?;

	let name = validate_name(name)?;
	if !validate::slug(slug) {
		return Err(ServerError::Validation(
			"Slug must be 1 to 60 lowercase letters, digits or hyphens, and not start or end with a hyphen."
				.to_owned(),
		));
	}

	super::insert(db, team, &name, slug).await.map_err(|e| {
		if crate::db::is_unique_violation(&e, SLUG_UNIQUE) {
			ServerError::Conflict("A project with that slug already exists.".to_owned())
		} else {
			e.into()
		}
	})
}

/// Lists a team's projects; needs `read project`.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<ProjectSummary>> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	Ok(super::list_by_team(db, team).await?)
}

/// One project; needs `read project`. A project of another team is
/// `ProjectNotFound`.
pub async fn find(db: &PgPool, user: UserId, team: TeamId, id: ProjectId) -> AppResult<Project> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	scope::project(db, team, id).await
}

/// One project; needs `read project`. A project of another team is `ProjectNotFound`.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ProjectId,
) -> AppResult<ProjectDetail> {
	let project = find(db, user, team, id).await?;
	Ok(ProjectDetail { project })
}

/// Deletes a project; needs `delete project`. Returns the deleted project.
pub async fn remove(db: &PgPool, user: UserId, team: TeamId, id: ProjectId) -> AppResult<Project> {
	authorize(db, user, team, Action::Delete, Resource::Project).await?;
	let project = scope::project(db, team, id).await?;
	super::delete(db, team, project.id).await?;
	Ok(project)
}
