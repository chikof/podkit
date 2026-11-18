//! Environment use cases shared by the JSON API and the web UI: authorize,
//! then scope, then validate, then touch the database. Environments belong to
//! a project, so they reuse the `project` permissions: `read` to look,
//! `create` to add, `update` to rename and reorder, `delete` to remove.

use sqlx::PgPool;

use super::Environment;
use crate::auth::permission::{Action, Resource, authorize};
use crate::db;
use crate::error::{AppResult, ServerError};
use crate::ids::{EnvironmentId, ProjectId, TeamId, UserId};
use crate::scope;
use crate::validate;

/// Constraint violated when a project reuses an environment slug.
const SLUG_UNIQUE: &str = "environments_project_id_slug_key";

const MAX_NAME_LEN: usize = 60;

/// Slugs the web UI keeps for its own pages under `/projects/{p}/`: an environment's page
/// is `/projects/{p}/{slug}`, so these would shadow it.
const RESERVED_SLUGS: &[&str] = &["apps", "databases", "models", "environments"];

/// How many resources a refusal to delete names before saying "and N more".
const LISTED: usize = 8;

fn validate_fields(name: &str, slug: &str) -> AppResult<String> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	if !validate::slug(slug) {
		return Err(ServerError::Validation(
			"Slug must be 1 to 60 lowercase letters, digits or hyphens, and not start or end with a hyphen."
				.to_owned(),
		));
	}
	if RESERVED_SLUGS.contains(&slug) {
		return Err(ServerError::Validation(format!(
			"The slug \"{slug}\" is reserved; pick another."
		)));
	}
	Ok(name.to_owned())
}

fn slug_conflict(e: sqlx::Error) -> ServerError {
	if db::is_unique_violation(&e, SLUG_UNIQUE) {
		ServerError::Conflict(
			"An environment with that slug already exists in this project.".to_owned(),
		)
	} else {
		e.into()
	}
}

/// Adds an environment at the end of the project's list; needs `create project`.
pub async fn create(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	name: &str,
	slug: &str,
) -> AppResult<Environment> {
	authorize(db, user, team, Action::Create, Resource::Project).await?;
	scope::project(db, team, project).await?;
	let name = validate_fields(name, slug)?;
	super::insert(db, project, &name, slug)
		.await
		.map_err(slug_conflict)
}

/// A project's environments in display order; needs `read project`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<Vec<Environment>> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	scope::project(db, team, project).await?;
	Ok(super::list_by_project(db, project).await?)
}

/// One environment; needs `read project`. An environment of another team or
/// project is `EnvironmentNotFound`.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: EnvironmentId,
) -> AppResult<Environment> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	scope::environment(db, team, project, id).await
}

/// Renames an environment, and changes its slug when `slug` is given; needs
/// `update project`. Resources keep pointing at it by id.
pub async fn rename(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: EnvironmentId,
	name: &str,
	slug: Option<&str>,
) -> AppResult<Environment> {
	authorize(db, user, team, Action::Update, Resource::Project).await?;
	let current = scope::environment(db, team, project, id).await?;
	let slug = slug.unwrap_or(&current.slug);
	let name = validate_fields(name, slug)?;
	super::update(db, current.id, &name, slug)
		.await
		.map_err(slug_conflict)
}

/// Puts the project's environments in the order given; needs `update project`.
/// `order` must name every environment of the project exactly once. The first
/// becomes the default environment of the resource endpoints.
pub async fn reorder(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	order: &[EnvironmentId],
) -> AppResult<Vec<Environment>> {
	authorize(db, user, team, Action::Update, Resource::Project).await?;
	scope::project(db, team, project).await?;

	let mut tx = db.begin().await?;
	let current = super::list_by_project(&mut *tx, project).await?;
	let mut want: Vec<_> = order.iter().map(|i| i.0).collect();
	let mut have: Vec<_> = current.iter().map(|e| e.id.0).collect();
	want.sort_unstable();
	have.sort_unstable();
	if want != have {
		return Err(ServerError::Validation(
			"The order must list every environment of the project exactly once.".to_owned(),
		));
	}
	for (position, id) in order.iter().enumerate() {
		let position = i32::try_from(position).unwrap_or(i32::MAX);
		super::set_position(&mut *tx, *id, position).await?;
	}
	let environments = super::list_by_project(&mut *tx, project).await?;
	tx.commit().await?;
	Ok(environments)
}

/// Deletes an environment; needs `delete project`. Refused with a 409 when the
/// environment still holds resources (the message names them) and when it is
/// the project's last environment.
pub async fn remove(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: EnvironmentId,
) -> AppResult<Environment> {
	authorize(db, user, team, Action::Delete, Resource::Project).await?;
	let environment = scope::environment(db, team, project, id).await?;

	if super::list_by_project(db, project).await?.len() <= 1 {
		return Err(ServerError::Conflict(
			"A project needs at least one environment, so the last one can't be deleted."
				.to_owned(),
		));
	}
	not_empty(db, &environment).await?;

	// A resource created after the check above trips the foreign key.
	super::delete(db, environment.id).await.map_err(|e| {
		if db::is_reference_violation(&e) {
			ServerError::Conflict(
				"The environment still has resources; move or delete them first.".to_owned(),
			)
		} else {
			e.into()
		}
	})?;
	Ok(environment)
}

async fn not_empty(db: &PgPool, environment: &Environment) -> AppResult<()> {
	let resources = super::resources(db, environment.id).await?;
	if resources.is_empty() {
		return Ok(());
	}
	let mut listed: Vec<String> = resources
		.iter()
		.take(LISTED)
		.map(|(kind, name)| format!("{kind} {name}"))
		.collect();
	if resources.len() > LISTED {
		listed.push(format!("and {} more", resources.len() - LISTED));
	}
	Err(ServerError::Conflict(format!(
		"The {} environment still has resources; move or delete them first: {}.",
		environment.name,
		listed.join(", ")
	)))
}

/// The environment a resource goes in: `requested` when given (it must belong
/// to `project`, else `EnvironmentNotFound`), otherwise the project's first.
/// No permission check: the resource's own service has already authorized the
/// caller and scoped the project.
pub async fn resolve(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	requested: Option<EnvironmentId>,
) -> AppResult<EnvironmentId> {
	match requested {
		Some(id) => Ok(scope::environment(db, team, project, id).await?.id),
		None => super::first(db, project)
			.await?
			.ok_or(ServerError::EnvironmentNotFound),
	}
}
