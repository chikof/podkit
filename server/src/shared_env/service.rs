//! Shared variable use cases for the web UI: authorize, then scope, then validate, then
//! touch the database.
//!
//! A secret's value goes in and never comes out. A plain (non-secret) variable's value is
//! returned only by [`list`], which is the page of the scope that owns it; the pages that
//! merely inherit a variable ([`inherited`]) only ever get names.

use std::collections::BTreeSet;

use sqlx::PgPool;

use super::resolve::{self, ResolveError};
use super::{Level, Scope};
use crate::auth::permission::{Action, Resource, authorize};
use crate::env_vars;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, SharedVarId, StackId, TeamId, UserId};
use crate::state::AppState;
use crate::{deployments, scope, stacks, validate};

const MAX_VALUE_BYTES: usize = 64 * 1024;

/// A shared variable as a page may show it.
#[derive(Debug, Clone)]
pub struct SharedVarView {
	pub id: SharedVarId,
	pub key: String,
	pub is_secret: bool,
	/// Only for a variable that is not a secret.
	pub value: Option<String>,
}

fn check_key(key: &str) -> AppResult<()> {
	if validate::env_key(key) {
		Ok(())
	} else {
		Err(ServerError::Validation(
			"Name must be letters, digits and underscores, and not start with a digit.".to_owned(),
		))
	}
}

/// Checks that `scope` exists in its team; another team's ids are a 404.
async fn check_scope(db: &PgPool, scope: Scope) -> AppResult<()> {
	match scope {
		Scope::Team(_) => {}
		Scope::Project(team, project) => {
			scope::project(db, team, project).await?;
		}
		Scope::Environment(team, project, env) => {
			scope::environment(db, team, project, env).await?;
		}
	}
	Ok(())
}

/// The variables set at exactly `scope`, ordered by key; needs `read env_var`.
pub async fn list(s: &AppState, user: UserId, scope: Scope) -> AppResult<Vec<SharedVarView>> {
	authorize(&s.db, user, scope.team(), Action::Read, Resource::EnvVar).await?;
	check_scope(&s.db, scope).await?;
	let rows = super::list_scope(&s.db, scope).await?;
	rows.into_iter()
		.map(|row| {
			let value = if row.is_secret {
				None
			} else {
				Some(s.secrets.decrypt(&row.value).map_err(|e| {
					tracing::error!(shared_var = row.id.0, "shared variable decrypt failed: {e}");
					ServerError::Internal
				})?)
			};
			Ok(SharedVarView {
				id: row.id,
				key: row.key,
				is_secret: row.is_secret,
				value,
			})
		})
		.collect()
}

/// Sets `key` at `scope`, creating it or replacing its value; needs `create env_var`.
pub async fn set(
	s: &AppState,
	user: UserId,
	scope: Scope,
	key: &str,
	value: &str,
	is_secret: bool,
) -> AppResult<SharedVarView> {
	authorize(&s.db, user, scope.team(), Action::Create, Resource::EnvVar).await?;
	check_scope(&s.db, scope).await?;
	check_key(key)?;
	if value.len() > MAX_VALUE_BYTES {
		return Err(ServerError::Validation(format!(
			"A value can be at most {} KiB.",
			MAX_VALUE_BYTES / 1024
		)));
	}
	if !resolve::refs(value).is_empty() {
		return Err(ServerError::Validation(
			"A shared variable can't refer to another one with ${{shared.…}}; put the reference in the application's own variable."
				.to_owned(),
		));
	}
	let encrypted = s
		.secrets
		.encrypt(value)
		.map_err(|e| ServerError::Validation(format!("failed to encrypt value: {e}")))?;
	let row = super::upsert(&s.db, scope, key, &encrypted, is_secret, user).await?;
	Ok(SharedVarView {
		id: row.id,
		key: row.key,
		is_secret: row.is_secret,
		value: None,
	})
}

/// Deletes a variable set at `scope`; needs `delete env_var`. Returns its name.
pub async fn remove(
	s: &AppState,
	user: UserId,
	scope: Scope,
	id: SharedVarId,
) -> AppResult<String> {
	authorize(&s.db, user, scope.team(), Action::Delete, Resource::EnvVar).await?;
	check_scope(&s.db, scope).await?;
	super::delete(&s.db, scope, id)
		.await?
		.ok_or(ServerError::EnvVarNotFound)
}

// --- who is affected -----------------------------------------------------------------

/// A resource a shared variable reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
	pub project: ProjectId,
	pub id: i64,
	pub name: String,
	pub kind: AffectedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffectedKind {
	Application,
	Stack,
}

/// The applications and stacks whose effective value of `key` comes from `scope`: those
/// below it that don't override the name with a variable of their own or of a nearer
/// scope. (A stack's own variables are encrypted as one blob, so every stack below the scope
/// is listed.) Needs `read env_var`.
pub async fn affected(
	db: &PgPool,
	user: UserId,
	scope: Scope,
	key: &str,
) -> AppResult<Vec<Affected>> {
	authorize(db, user, scope.team(), Action::Read, Resource::EnvVar).await?;
	check_scope(db, scope).await?;
	let (project, environment): (Option<ProjectId>, Option<EnvironmentId>) = match scope {
		Scope::Team(_) => (None, None),
		Scope::Project(_, p) => (Some(p), None),
		Scope::Environment(_, p, e) => (Some(p), Some(e)),
	};
	let level: i32 = match scope.level() {
		Level::Team => 0,
		Level::Project => 1,
		Level::Environment => 2,
	};
	let shadowed = "NOT EXISTS (
			SELECT 1 FROM shared_env_vars sv
			WHERE sv.team_id = p.team_id AND sv.key = $4
			  AND ((sv.environment_id = r.environment_id AND $5 < 2)
			    OR (sv.project_id = r.project_id AND sv.environment_id IS NULL AND $5 < 1)))";
	let scoped = "p.team_id = $1 AND ($2::bigint IS NULL OR r.project_id = $2)
		AND ($3::bigint IS NULL OR r.environment_id = $3)";

	let apps: Vec<(i64, ProjectId, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT r.id, r.project_id, r.name FROM applications r
		 JOIN projects p ON p.id = r.project_id
		 WHERE {scoped} AND {shadowed}
		   AND NOT EXISTS (SELECT 1 FROM env_vars ev WHERE ev.application_id = r.id AND ev.key = $4)
		 ORDER BY r.name, r.id"
	)))
	.bind(scope.team())
	.bind(project)
	.bind(environment)
	.bind(key)
	.bind(level)
	.fetch_all(db)
	.await?;
	let stack_rows: Vec<(i64, ProjectId, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT r.id, r.project_id, r.name FROM stacks r
		 JOIN projects p ON p.id = r.project_id
		 WHERE {scoped} AND {shadowed}
		 ORDER BY r.name, r.id"
	)))
	.bind(scope.team())
	.bind(project)
	.bind(environment)
	.bind(key)
	.bind(level)
	.fetch_all(db)
	.await?;

	let mut out: Vec<Affected> = apps
		.into_iter()
		.map(|(id, project, name)| Affected {
			project,
			id,
			name,
			kind: AffectedKind::Application,
		})
		.collect();
	out.extend(stack_rows.into_iter().map(|(id, project, name)| Affected {
		project,
		id,
		name,
		kind: AffectedKind::Stack,
	}));
	Ok(out)
}

/// What "redeploy affected" did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Redeployed {
	pub queued: usize,
	/// Resources the caller may not deploy, or whose deploy could not be queued.
	pub skipped: usize,
}

/// Queues a deploy of every resource [`affected`] by `key` at `scope`. Each one goes through
/// the normal deploy use case, so its own permission applies.
pub async fn redeploy_affected(
	s: &AppState,
	user: UserId,
	scope: Scope,
	key: &str,
) -> AppResult<Redeployed> {
	let list = affected(&s.db, user, scope, key).await?;
	let team = scope.team();
	let mut done = Redeployed::default();
	for item in list {
		let result = match item.kind {
			AffectedKind::Application => {
				deployments::service::deploy(s, user, team, item.project, ApplicationId(item.id))
					.await
					.map(drop)
			}
			AffectedKind::Stack => {
				stacks::service::deploy(s, user, team, item.project, StackId(item.id))
					.await
					.map(drop)
			}
		};
		match result {
			Ok(()) => done.queued += 1,
			Err(e) => {
				tracing::warn!("redeploy of an affected resource skipped: {e}");
				done.skipped += 1;
			}
		}
	}
	Ok(done)
}

// --- an application's view -------------------------------------------------------------

/// One inherited variable of an application: its name, where it comes from and whether the
/// application's own variable of that name replaces it. Never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inherited {
	pub key: String,
	pub level: Level,
	pub overridden: bool,
}

/// What `app` inherits, ordered by key; needs `read env_var`.
pub async fn inherited(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<Inherited>> {
	authorize(&s.db, user, team, Action::Read, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let own: BTreeSet<String> = env_vars::list(&s.db, app.id)
		.await?
		.into_iter()
		.map(|v| v.key)
		.collect();
	let rows = super::visible(&s.db, team, app.project_id, app.environment_id).await?;
	let mut winners: std::collections::BTreeMap<String, Level> = std::collections::BTreeMap::new();
	for row in &rows {
		winners.insert(row.key.clone(), row.level());
	}
	Ok(winners
		.into_iter()
		.map(|(key, level)| Inherited {
			overridden: own.contains(&key),
			key,
			level,
		})
		.collect())
}

/// Refuses a value of an application's variable that refers to a shared name nothing
/// sets, with a message naming the missing ones. Called when the value is saved.
pub async fn validate_refs(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	environment: EnvironmentId,
	key: &str,
	value: &str,
) -> AppResult<()> {
	let wanted = resolve::refs(value);
	if wanted.is_empty() {
		return Ok(());
	}
	let known = resolve::shared_keys(db, team, project, environment).await?;
	let missing: Vec<String> = wanted.into_iter().filter(|k| !known.contains(k)).collect();
	if missing.is_empty() {
		return Ok(());
	}
	Err(ServerError::Validation(
		ResolveError::Missing {
			owner: format!("env var {key}"),
			keys: missing,
		}
		.to_string(),
	))
}

// --- naming a scope ----------------------------------------------------------------------

/// What a page needs to name the scope it shows.
#[derive(Debug, Clone)]
pub struct ScopeInfo {
	pub project: Option<(ProjectId, String)>,
	pub environment: Option<(EnvironmentId, String)>,
	/// The project's environments, for the page's tabs; empty at team scope.
	pub environments: Vec<(EnvironmentId, String)>,
}

/// Names `scope` and lists its siblings; needs `read env_var`. A scope of another team is
/// a 404.
pub async fn context(db: &PgPool, user: UserId, scope: Scope) -> AppResult<ScopeInfo> {
	authorize(db, user, scope.team(), Action::Read, Resource::EnvVar).await?;
	let (team, project, environment) = match scope {
		Scope::Team(_) => {
			return Ok(ScopeInfo {
				project: None,
				environment: None,
				environments: Vec::new(),
			});
		}
		Scope::Project(t, p) => (t, p, None),
		Scope::Environment(t, p, e) => (t, p, Some(e)),
	};
	let project = scope::project(db, team, project).await?;
	let environment = match environment {
		Some(id) => Some(scope::environment(db, team, project.id, id).await?),
		None => None,
	};
	let environments = crate::environments::list_by_project(db, project.id)
		.await?
		.into_iter()
		.map(|e| (e.id, e.name))
		.collect();
	Ok(ScopeInfo {
		project: Some((project.id, project.name)),
		environment: environment.map(|e| (e.id, e.name)),
		environments,
	})
}
