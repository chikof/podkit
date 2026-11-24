//! The search use case: one team, one query, the hits the caller may see.

use sqlx::PgPool;

use super::{Hit, Kind};
use crate::auth::permission::{self, Action, Resource};
use crate::error::{AppResult, ServerError};
use crate::ids::{EnvironmentId, ProjectId, TeamId, UserId};

/// Most hits of one kind.
const PER_KIND: i64 = 6;
/// Longest query that is searched; the rest is ignored.
const MAX_QUERY: usize = 80;

/// `%query%` for `ILIKE`, with the wildcard characters in the query made literal.
fn pattern(query: &str) -> String {
	let mut out = String::with_capacity(query.len() + 2);
	out.push('%');
	for c in query.chars().take(MAX_QUERY) {
		if matches!(c, '%' | '_' | '\\') {
			out.push('\\');
		}
		out.push(c);
	}
	out.push('%');
	out
}

/// Projects, environments, applications, databases, models and servers of `team` whose
/// name matches `query`, kind by kind, at most a handful of each. A blank query matches
/// nothing. A kind the caller may not read is skipped silently.
///
/// # Errors
/// `NotFound` when `user` is not in `team`, exactly as for a team that does not exist.
#[allow(
	clippy::too_many_lines,
	reason = "six near-identical queries, one per kind; sqlx::query! cannot share a body"
)]
pub async fn search(db: &PgPool, user: UserId, team: TeamId, query: &str) -> AppResult<Vec<Hit>> {
	let perms = permission::granted(db, user, team).await?;
	if !perms.is_member() {
		return Err(ServerError::NotFound);
	}
	let query = query.trim();
	if query.is_empty() {
		return Ok(Vec::new());
	}
	let like = pattern(query);
	let mut hits = Vec::new();

	if perms.can(Action::Read, Resource::Project) {
		let rows = sqlx::query!(
			r#"
				SELECT id AS "id: ProjectId", name
				FROM projects
				WHERE team_id = $1 AND (name ILIKE $2 OR slug ILIKE $2)
				ORDER BY name, id
				LIMIT $3
			"#,
			team as TeamId,
			like,
			PER_KIND
		)
		.fetch_all(db)
		.await?;
		hits.extend(rows.into_iter().map(|r| Hit {
			kind: Kind::Project,
			id: r.id.0,
			name: r.name,
			project: None,
			environment: None,
		}));

		let rows = sqlx::query!(
			r#"
				SELECT e.id AS "id: EnvironmentId", e.name,
					p.id AS "project_id: ProjectId", p.name AS project_name
				FROM environments e
				JOIN projects p ON p.id = e.project_id
				WHERE p.team_id = $1 AND e.name ILIKE $2
				ORDER BY e.name, e.id
				LIMIT $3
			"#,
			team as TeamId,
			like,
			PER_KIND
		)
		.fetch_all(db)
		.await?;
		hits.extend(rows.into_iter().map(|r| Hit {
			kind: Kind::Environment,
			id: r.id.0,
			name: r.name,
			project: Some((r.project_id, r.project_name)),
			environment: None,
		}));
	}

	if perms.can(Action::Read, Resource::Application) {
		let rows = sqlx::query!(
			r#"
				SELECT a.id, a.name, p.id AS "project_id: ProjectId", p.name AS project_name,
					e.id AS "environment_id: EnvironmentId", e.name AS environment_name
				FROM applications a
				JOIN projects p ON p.id = a.project_id
				JOIN environments e ON e.id = a.environment_id
				WHERE p.team_id = $1 AND (a.name ILIKE $2 OR a.slug ILIKE $2)
				ORDER BY a.name, a.id
				LIMIT $3
			"#,
			team as TeamId,
			like,
			PER_KIND
		)
		.fetch_all(db)
		.await?;
		hits.extend(rows.into_iter().map(|r| Hit {
			kind: Kind::Application,
			id: r.id,
			name: r.name,
			project: Some((r.project_id, r.project_name)),
			environment: Some((r.environment_id, r.environment_name)),
		}));
	}

	if perms.can(Action::Read, Resource::Database) {
		let rows = sqlx::query!(
			r#"
				SELECT d.id, d.name, p.id AS "project_id: ProjectId", p.name AS project_name,
					e.id AS "environment_id: EnvironmentId", e.name AS environment_name
				FROM databases d
				JOIN projects p ON p.id = d.project_id
				JOIN environments e ON e.id = d.environment_id
				WHERE p.team_id = $1 AND d.name ILIKE $2
				ORDER BY d.name, d.id
				LIMIT $3
			"#,
			team as TeamId,
			like,
			PER_KIND
		)
		.fetch_all(db)
		.await?;
		hits.extend(rows.into_iter().map(|r| Hit {
			kind: Kind::Database,
			id: r.id,
			name: r.name,
			project: Some((r.project_id, r.project_name)),
			environment: Some((r.environment_id, r.environment_name)),
		}));
	}

	if perms.can(Action::Read, Resource::Model) {
		let rows = sqlx::query!(
			r#"
				SELECT m.id, m.name, p.id AS "project_id: ProjectId", p.name AS project_name,
					e.id AS "environment_id: EnvironmentId", e.name AS environment_name
				FROM models m
				JOIN projects p ON p.id = m.project_id
				JOIN environments e ON e.id = m.environment_id
				WHERE p.team_id = $1 AND m.name ILIKE $2
				ORDER BY m.name, m.id
				LIMIT $3
			"#,
			team as TeamId,
			like,
			PER_KIND
		)
		.fetch_all(db)
		.await?;
		hits.extend(rows.into_iter().map(|r| Hit {
			kind: Kind::Model,
			id: r.id,
			name: r.name,
			project: Some((r.project_id, r.project_name)),
			environment: Some((r.environment_id, r.environment_name)),
		}));
	}

	if perms.can(Action::Read, Resource::Server) {
		let rows = sqlx::query!(
			r#"
				SELECT id, name
				FROM servers
				WHERE team_id = $1 AND name ILIKE $2
				ORDER BY name, id
				LIMIT $3
			"#,
			team as TeamId,
			like,
			PER_KIND
		)
		.fetch_all(db)
		.await?;
		hits.extend(rows.into_iter().map(|r| Hit {
			kind: Kind::Server,
			id: r.id,
			name: r.name,
			project: None,
			environment: None,
		}));
	}

	Ok(hits)
}

#[cfg(test)]
mod unit {
	use super::pattern;

	#[test]
	fn wildcards_in_the_query_are_literal() {
		assert_eq!(pattern("api"), "%api%");
		assert_eq!(pattern("100%_x\\"), "%100\\%\\_x\\\\%");
	}

	#[test]
	fn a_very_long_query_is_cut() {
		let long = "a".repeat(500);
		assert_eq!(pattern(&long).len(), 82);
	}
}
