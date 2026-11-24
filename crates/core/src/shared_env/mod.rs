//! Shared environment variables: set once at team, project or environment scope and
//! inherited by the applications and stacks below. Precedence, lowest first: team, project,
//! environment, then the application's own variables. Values are encrypted at rest with
//! `state.secrets`; a secret's value is never returned.

use sqlx::PgExecutor;

use crate::ids::{EnvironmentId, ProjectId, SharedVarId, TeamId, UserId};

pub mod resolve;
pub mod service;

#[cfg(test)]
mod tests;

/// Where a shared variable lives. Also the order of precedence: later wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
	Team,
	Project,
	Environment,
}

impl Level {
	pub const fn label(self) -> &'static str {
		match self {
			Self::Team => "team",
			Self::Project => "project",
			Self::Environment => "environment",
		}
	}
}

/// One scope a variable can be set at, with the ids that locate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
	Team(TeamId),
	Project(TeamId, ProjectId),
	Environment(TeamId, ProjectId, EnvironmentId),
}

impl Scope {
	pub const fn level(self) -> Level {
		match self {
			Self::Team(_) => Level::Team,
			Self::Project(..) => Level::Project,
			Self::Environment(..) => Level::Environment,
		}
	}

	pub const fn team(self) -> TeamId {
		match self {
			Self::Team(t) | Self::Project(t, _) | Self::Environment(t, _, _) => t,
		}
	}
}

/// Row from the `shared_env_vars` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SharedVar {
	pub id: SharedVarId,
	pub project_id: Option<ProjectId>,
	pub environment_id: Option<EnvironmentId>,
	pub key: String,
	/// age-encrypted value; never stored or returned as plaintext.
	pub value: Vec<u8>,
	pub is_secret: bool,
	/// A secret that previews of pull requests may read.
	pub preview: bool,
}

impl SharedVar {
	pub const fn level(&self) -> Level {
		if self.environment_id.is_some() {
			Level::Environment
		} else if self.project_id.is_some() {
			Level::Project
		} else {
			Level::Team
		}
	}
}

const COLUMNS: &str = "id, team_id, project_id, environment_id, key, value, is_secret, preview, created_by, updated_at";

/// The variables set exactly at `scope`, ordered by key.
pub async fn list_scope(
	db: impl PgExecutor<'_>,
	scope: Scope,
) -> Result<Vec<SharedVar>, sqlx::Error> {
	let (team, project, environment) = ids(scope);
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM shared_env_vars
		 WHERE team_id = $1
		   AND project_id IS NOT DISTINCT FROM $2
		   AND environment_id IS NOT DISTINCT FROM $3
		 ORDER BY key"
	)))
	.bind(team)
	.bind(project)
	.bind(environment)
	.fetch_all(db)
	.await
}

/// Every variable that applies to a resource of `environment`: team, project and that
/// environment's own, lowest precedence first.
pub async fn visible(
	db: impl PgExecutor<'_>,
	team: TeamId,
	project: ProjectId,
	environment: EnvironmentId,
) -> Result<Vec<SharedVar>, sqlx::Error> {
	let mut rows: Vec<SharedVar> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLUMNS} FROM shared_env_vars
		 WHERE team_id = $1
		   AND (project_id IS NULL OR project_id = $2)
		   AND (environment_id IS NULL OR environment_id = $3)"
	)))
	.bind(team)
	.bind(project)
	.bind(environment)
	.fetch_all(db)
	.await?;
	rows.sort_by(|a, b| a.level().cmp(&b.level()).then_with(|| a.key.cmp(&b.key)));
	Ok(rows)
}

fn ids(scope: Scope) -> (TeamId, Option<ProjectId>, Option<EnvironmentId>) {
	match scope {
		Scope::Team(t) => (t, None, None),
		Scope::Project(t, p) => (t, Some(p), None),
		Scope::Environment(t, p, e) => (t, Some(p), Some(e)),
	}
}

/// Sets `key` at `scope`, replacing the value (and flag) when it already exists.
pub async fn upsert(
	db: impl PgExecutor<'_>,
	scope: Scope,
	key: &str,
	value: &[u8],
	is_secret: bool,
	user: UserId,
) -> Result<SharedVar, sqlx::Error> {
	let (team, project, environment) = ids(scope);
	let target = match scope {
		Scope::Team(_) => "(team_id, key) WHERE project_id IS NULL",
		Scope::Project(..) => {
			"(project_id, key) WHERE project_id IS NOT NULL AND environment_id IS NULL"
		}
		Scope::Environment(..) => "(environment_id, key) WHERE environment_id IS NOT NULL",
	};
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"INSERT INTO shared_env_vars
		   (id, team_id, project_id, environment_id, key, value, is_secret, created_by)
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
		 ON CONFLICT {target}
		 DO UPDATE SET value = EXCLUDED.value, is_secret = EXCLUDED.is_secret, updated_at = NOW()
		 RETURNING {COLUMNS}"
	)))
	.bind(SharedVarId::generate())
	.bind(team)
	.bind(project)
	.bind(environment)
	.bind(key)
	.bind(value)
	.bind(is_secret)
	.bind(user)
	.fetch_one(db)
	.await
}

/// Deletes variable `id` if it is set at exactly `scope`; returns the removed row's key.
pub async fn delete(
	db: impl PgExecutor<'_>,
	scope: Scope,
	id: SharedVarId,
) -> Result<Option<String>, sqlx::Error> {
	let (team, project, environment) = ids(scope);
	sqlx::query_scalar(
		"DELETE FROM shared_env_vars
		 WHERE id = $1 AND team_id = $2
		   AND project_id IS NOT DISTINCT FROM $3
		   AND environment_id IS NOT DISTINCT FROM $4
		 RETURNING key",
	)
	.bind(id)
	.bind(team)
	.bind(project)
	.bind(environment)
	.fetch_optional(db)
	.await
}

/// Marks a variable set at exactly `scope` as available (or not) to previews; false when it is
/// not set there.
pub async fn set_preview(
	db: impl PgExecutor<'_>,
	scope: Scope,
	id: SharedVarId,
	available: bool,
) -> Result<bool, sqlx::Error> {
	let (team, project, environment) = ids(scope);
	let done = sqlx::query(
		"UPDATE shared_env_vars SET preview = $5
		 WHERE id = $1 AND team_id = $2
		   AND project_id IS NOT DISTINCT FROM $3
		   AND environment_id IS NOT DISTINCT FROM $4",
	)
	.bind(id)
	.bind(team)
	.bind(project)
	.bind(environment)
	.bind(available)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}
