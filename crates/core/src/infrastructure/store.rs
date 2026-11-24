//! The SQL of the infrastructure slice: counts for the setup checklist, the placement of
//! every resource, scanner state per server, and the stored setup progress. Callers are
//! `service`, which authorizes first; every query is scoped by `team_id`.

use sqlx::PgPool;

use super::setup::{Facts, Item, Progress};
use crate::ids::{ServerId, TeamId};

pub async fn progress(db: &PgPool, team: TeamId) -> Result<Progress, sqlx::Error> {
	let row = sqlx::query!(
		r#"
			SELECT dismissed_at IS NOT NULL AS "dismissed!", skipped
			FROM infrastructure_setup WHERE team_id = $1
		"#,
		team as TeamId,
	)
	.fetch_optional(db)
	.await?;
	Ok(row.map_or_else(Progress::default, |r| Progress {
		dismissed: r.dismissed,
		skipped: r.skipped,
	}))
}

pub async fn set_dismissed(db: &PgPool, team: TeamId, dismissed: bool) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO infrastructure_setup (team_id, dismissed_at)
			VALUES ($1, CASE WHEN $2 THEN NOW() END)
			ON CONFLICT (team_id) DO UPDATE
			SET dismissed_at = CASE WHEN $2 THEN NOW() END, updated_at = NOW()
		"#,
		team as TeamId,
		dismissed,
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_skipped(
	db: &PgPool,
	team: TeamId,
	item: Item,
	skipped: bool,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO infrastructure_setup (team_id, skipped)
			VALUES ($1, CASE WHEN $3 THEN ARRAY[$2::text] ELSE '{}'::text[] END)
			ON CONFLICT (team_id) DO UPDATE
			SET skipped = CASE
					WHEN $3 THEN array_append(array_remove(infrastructure_setup.skipped, $2::text), $2::text)
					ELSE array_remove(infrastructure_setup.skipped, $2::text)
				END,
				updated_at = NOW()
		"#,
		team as TeamId,
		item.slug(),
		skipped,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// What the team has, in one round trip.
pub async fn facts(db: &PgPool, team: TeamId) -> Result<Facts, sqlx::Error> {
	let r = sqlx::query!(
		r#"
			SELECT
				(SELECT COUNT(*) FROM servers WHERE team_id = $1) AS "servers!",
				(SELECT COUNT(*) FROM servers WHERE team_id = $1 AND status = 'active') AS "servers_active!",
				(SELECT COUNT(*) FROM servers WHERE team_id = $1 AND status <> 'active') AS "servers_attention!",
				(SELECT COUNT(*) FROM applications a JOIN projects p ON p.id = a.project_id
					WHERE p.team_id = $1 AND (
						SELECT d.status FROM deployments d WHERE d.application_id = a.id
						ORDER BY d.created_at DESC, d.id DESC LIMIT 1
					) = 'running') AS "running_apps!",
				(SELECT COUNT(*) FROM networks WHERE team_id = $1 AND kind = 'user') AS "user_networks!",
				(SELECT COUNT(*) FROM ssh_keys WHERE team_id = $1) AS "ssh_keys!",
				(SELECT COUNT(*) FROM git_providers WHERE team_id = $1) AS "git_providers!",
				(SELECT COUNT(*) FROM registries WHERE team_id = $1) AS "registries!",
				(SELECT COUNT(*) FROM servers WHERE team_id = $1 AND scan_enabled) AS "scan_servers!",
				EXISTS (SELECT 1 FROM image_scans WHERE team_id = $1 AND db_built IS NOT NULL) AS "scan_db_ready!",
				(SELECT COUNT(*) FROM projects WHERE team_id = $1) AS "projects!",
				(SELECT COUNT(*) FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1)
				+ (SELECT COUNT(*) FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1)
				+ (SELECT COUNT(*) FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1)
				+ (SELECT COUNT(*) FROM stacks s JOIN projects p ON p.id = s.project_id WHERE p.team_id = $1) AS "resources!"
		"#,
		team as TeamId,
	)
	.fetch_one(db)
	.await?;
	Ok(Facts {
		servers: r.servers,
		servers_active: r.servers_active,
		servers_attention: r.servers_attention,
		running_apps: r.running_apps,
		user_networks: r.user_networks,
		ssh_keys: r.ssh_keys,
		git_providers: r.git_providers,
		registries: r.registries,
		scan_servers: r.scan_servers,
		scan_db_ready: r.scan_db_ready,
		projects: r.projects,
		resources: r.resources,
	})
}

/// One resource and where it is: the row of the "what runs where" table.
#[derive(Debug, Clone)]
pub struct Placed {
	pub kind: String,
	pub id: i64,
	pub name: String,
	pub slug: String,
	pub project_id: i64,
	pub project_name: String,
	pub environment_name: String,
	pub server_id: ServerId,
	/// A deployment status, a database, model or stack status, or `not deployed`.
	pub status: String,
}

pub async fn placements(db: &PgPool, team: TeamId) -> Result<Vec<Placed>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT r.kind AS "kind!", r.id AS "id!", r.name AS "name!", r.slug AS "slug!",
				r.project_id AS "project_id!", p.name AS "project_name!",
				e.name AS "environment_name!",
				r.server_id AS "server_id!: ServerId", r.status AS "status!"
			FROM (
				SELECT 'application' AS kind, a.id, a.name, a.slug, a.project_id, a.environment_id, a.server_id,
					COALESCE((
						SELECT d.status FROM deployments d WHERE d.application_id = a.id
						ORDER BY d.created_at DESC, d.id DESC LIMIT 1
					), 'not deployed') AS status
				FROM applications a
				UNION ALL
				SELECT 'database', d.id, d.name, '', d.project_id, d.environment_id, d.server_id, d.status
				FROM databases d
				UNION ALL
				SELECT 'model', m.id, m.name, m.slug, m.project_id, m.environment_id, m.server_id, m.status
				FROM models m
				UNION ALL
				SELECT 'stack', s.id, s.name, s.slug, s.project_id, s.environment_id, s.server_id, s.status
				FROM stacks s
			) r
			JOIN projects p ON p.id = r.project_id
			JOIN environments e ON e.id = r.environment_id
			WHERE p.team_id = $1
			ORDER BY p.name, p.id, e.position, e.id, r.kind, r.name, r.id
		"#,
		team as TeamId,
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| Placed {
			kind: r.kind,
			id: r.id,
			name: r.name,
			slug: r.slug,
			project_id: r.project_id,
			project_name: r.project_name,
			environment_name: r.environment_name,
			server_id: r.server_id,
			status: r.status,
		})
		.collect())
}

/// Scanner state of one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerRow {
	pub server_id: ServerId,
	pub enabled: bool,
	/// Scans that finished, so the database behind them was downloaded.
	pub scanned: i64,
}

pub async fn scanners(db: &PgPool, team: TeamId) -> Result<Vec<ScannerRow>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT s.id AS "id: ServerId", s.scan_enabled,
				COUNT(i.id) FILTER (WHERE i.db_built IS NOT NULL) AS "scanned!"
			FROM servers s LEFT JOIN image_scans i ON i.server_id = s.id
			WHERE s.team_id = $1
			GROUP BY s.id
		"#,
		team as TeamId,
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| ScannerRow {
			server_id: r.id,
			enabled: r.scan_enabled,
			scanned: r.scanned,
		})
		.collect())
}
