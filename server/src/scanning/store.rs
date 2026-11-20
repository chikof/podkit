//! Rows of `image_scans`, `scan_findings`, `scan_ignores` and `scan_links`.
//! Nothing here checks who is asking: callers are the job and `service`.

use std::collections::HashMap;

use proto::{ScanFinding, ScanResult};
use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

use super::{Badge, BadgeState, CACHE_FOR, Counts, ResourceKind, ScanStatus, Severity, is_act_now};
use crate::ids::{DeploymentId, ScanId, ScanIgnoreId, ServerId, TeamId, UserId};

/// A scan, without its SBOM. The stored counts and the time are read by the tests; the pages
/// take the team's counts (ignores applied) from the findings instead.
#[cfg_attr(
	not(test),
	allow(dead_code, reason = "the stored counts are only checked by the tests")
)]
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Scan {
	pub id: ScanId,
	pub team_id: TeamId,
	pub server_id: ServerId,
	pub image_digest: String,
	pub image_ref: String,
	pub status: ScanStatus,
	pub started_at: Option<OffsetDateTime>,
	pub finished_at: Option<OffsetDateTime>,
	pub rescanned_at: Option<OffsetDateTime>,
	pub sbom_bytes: Option<i32>,
	pub force: bool,
	pub critical: i32,
	pub high: i32,
	pub act_now: i32,
	pub db_built: Option<String>,
	pub tools: Option<String>,
	pub error: Option<String>,
}

const SCAN_COLUMNS: &str = "s.id, s.team_id, s.server_id, s.image_digest, s.image_ref, s.status,
	s.started_at, s.finished_at, s.rescanned_at, s.sbom_bytes, s.force, s.critical, s.high,
	s.act_now, s.db_built, s.tools, s.error";

/// One finding as stored, with whether the team has ignored it right now.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredFinding {
	pub vuln_id: String,
	pub package: String,
	pub version: String,
	pub fixed_version: Option<String>,
	pub severity: String,
	pub cvss: Option<f64>,
	pub epss: Option<f64>,
	pub kev: bool,
	pub act_now: bool,
	pub url: Option<String>,
	pub ignored: bool,
}

/// What the findings table can be narrowed to.
#[derive(Debug, Clone, Copy, Default)]
pub struct FindingFilter {
	pub severity: Option<Severity>,
	pub act_now: bool,
	pub fixable: bool,
	pub show_ignored: bool,
}

/// An ignore as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredIgnore {
	pub id: ScanIgnoreId,
	pub vuln_id: String,
	pub package: Option<String>,
	pub reason: String,
	pub expires_at: OffsetDateTime,
	pub created_by_name: Option<String>,
	pub created_at: OffsetDateTime,
}

/// A server's scan settings.
#[derive(Debug, Clone, Copy, PartialEq, sqlx::FromRow)]
pub struct ServerScanSettings {
	pub team_id: TeamId,
	pub scan_enabled: bool,
	pub scan_cpus: f64,
	pub scan_memory_mb: i32,
}

pub async fn server_settings(
	db: impl PgExecutor<'_>,
	server: ServerId,
) -> Result<Option<ServerScanSettings>, sqlx::Error> {
	sqlx::query_as(
		"SELECT team_id, scan_enabled, scan_cpus, scan_memory_mb FROM servers WHERE id = $1",
	)
	.bind(server)
	.fetch_optional(db)
	.await
}

pub async fn insert_queued(
	db: impl PgExecutor<'_>,
	team: TeamId,
	server: ServerId,
	image_ref: &str,
	force: bool,
) -> Result<ScanId, sqlx::Error> {
	let id = ScanId::generate();
	sqlx::query(
		"INSERT INTO image_scans (id, team_id, server_id, image_ref, force) VALUES ($1, $2, $3, $4, $5)",
	)
	.bind(id)
	.bind(team)
	.bind(server)
	.bind(image_ref)
	.bind(force)
	.execute(db)
	.await?;
	Ok(id)
}

/// Makes `scan` what the resource shows, from `deployment`.
pub async fn link(
	db: impl PgExecutor<'_>,
	kind: ResourceKind,
	resource: i64,
	scan: ScanId,
	deployment: Option<DeploymentId>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO scan_links (resource_kind, resource_id, scan_id, deployment_id)
		VALUES ($1, $2, $3, $4)
		ON CONFLICT (resource_kind, resource_id, scan_id)
		DO UPDATE SET deployment_id = EXCLUDED.deployment_id, created_at = NOW()",
	)
	.bind(kind.as_str())
	.bind(resource)
	.bind(scan)
	.bind(deployment)
	.execute(db)
	.await
	.map(drop)
}

pub async fn find(db: impl PgExecutor<'_>, id: ScanId) -> Result<Option<Scan>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {SCAN_COLUMNS} FROM image_scans s WHERE s.id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// `find`, but only inside `team`: another team's scan does not exist.
pub async fn find_in_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: ScanId,
) -> Result<Option<Scan>, sqlx::Error> {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {SCAN_COLUMNS} FROM image_scans s WHERE s.id = $1 AND s.team_id = $2"
	)))
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await
}

/// A finished scan of `digest` on `server` that is recent enough to reuse.
pub async fn find_cached(
	db: impl PgExecutor<'_>,
	server: ServerId,
	digest: &str,
	exclude: ScanId,
	now: OffsetDateTime,
) -> Result<Option<ScanId>, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT id FROM image_scans
		WHERE server_id = $1 AND image_digest = $2 AND id <> $3 AND status = 'succeeded'
			AND finished_at > $4
		ORDER BY finished_at DESC LIMIT 1",
	)
	.bind(server)
	.bind(digest)
	.bind(exclude)
	.bind(now - CACHE_FOR)
	.fetch_optional(db)
	.await
}

/// Points every link of `from` at `to` and deletes `from`: the placeholder of a
/// scan that turned out to be a repeat.
pub async fn merge_into(db: &PgPool, from: ScanId, to: ScanId) -> Result<(), sqlx::Error> {
	let mut tx = db.begin().await?;
	sqlx::query(
		"INSERT INTO scan_links (resource_kind, resource_id, scan_id, deployment_id, created_at)
		SELECT resource_kind, resource_id, $2, deployment_id, created_at FROM scan_links WHERE scan_id = $1
		ON CONFLICT (resource_kind, resource_id, scan_id)
		DO UPDATE SET deployment_id = EXCLUDED.deployment_id, created_at = EXCLUDED.created_at",
	)
	.bind(from)
	.bind(to)
	.execute(&mut *tx)
	.await?;
	sqlx::query("DELETE FROM image_scans WHERE id = $1")
		.bind(from)
		.execute(&mut *tx)
		.await?;
	tx.commit().await
}

pub async fn mark_scanning(db: impl PgExecutor<'_>, id: ScanId) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE image_scans SET status = 'scanning', started_at = NOW(), error = NULL WHERE id = $1",
	)
	.bind(id)
	.execute(db)
	.await
	.map(drop)
}

pub async fn set_digest(
	db: impl PgExecutor<'_>,
	id: ScanId,
	digest: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE image_scans SET image_digest = $2 WHERE id = $1")
		.bind(id)
		.bind(digest)
		.execute(db)
		.await
		.map(drop)
}

/// Stores a scan's outcome: findings, counts and (when the agent built one) the SBOM.
pub async fn store_result(
	db: &PgPool,
	id: ScanId,
	result: &ScanResult,
	sbom: Option<Vec<u8>>,
	sbom_bytes: Option<i32>,
	rescan: bool,
) -> Result<(), sqlx::Error> {
	let counts = Counts::of(&result.findings);
	let mut tx = db.begin().await?;
	replace_findings(&mut tx, id, &result.findings).await?;
	sqlx::query(
		"UPDATE image_scans SET
			status = 'succeeded', error = NULL,
			finished_at = CASE WHEN $2 THEN finished_at ELSE NOW() END,
			rescanned_at = CASE WHEN $2 THEN NOW() ELSE rescanned_at END,
			sbom = COALESCE($3, sbom), sbom_bytes = COALESCE($4, sbom_bytes),
			critical = $5, high = $6, medium = $7, low = $8, unknown = $9, act_now = $10,
			db_built = $11, tools = $12
		WHERE id = $1",
	)
	.bind(id)
	.bind(rescan)
	.bind(sbom)
	.bind(sbom_bytes)
	.bind(i32::try_from(counts.critical).unwrap_or(i32::MAX))
	.bind(i32::try_from(counts.high).unwrap_or(i32::MAX))
	.bind(i32::try_from(counts.medium).unwrap_or(i32::MAX))
	.bind(i32::try_from(counts.low).unwrap_or(i32::MAX))
	.bind(i32::try_from(counts.unknown).unwrap_or(i32::MAX))
	.bind(i32::try_from(counts.act_now).unwrap_or(i32::MAX))
	.bind(&result.db_built)
	.bind(&result.tools)
	.execute(&mut *tx)
	.await?;
	tx.commit().await
}

async fn replace_findings(
	tx: &mut sqlx::PgConnection,
	id: ScanId,
	findings: &[ScanFinding],
) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM scan_findings WHERE scan_id = $1")
		.bind(id)
		.execute(&mut *tx)
		.await?;
	if findings.is_empty() {
		return Ok(());
	}
	sqlx::query(
		"INSERT INTO scan_findings
			(scan_id, vuln_id, package, version, fixed_version, severity, cvss, epss, kev, act_now, url)
		SELECT $1, * FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[],
			$7::float8[], $8::float8[], $9::bool[], $10::bool[], $11::text[])
		ON CONFLICT DO NOTHING",
	)
	.bind(id)
	.bind(
		findings
			.iter()
			.map(|f| f.vuln_id.clone())
			.collect::<Vec<_>>(),
	)
	.bind(
		findings
			.iter()
			.map(|f| f.package.clone())
			.collect::<Vec<_>>(),
	)
	.bind(
		findings
			.iter()
			.map(|f| f.version.clone())
			.collect::<Vec<_>>(),
	)
	.bind(
		findings
			.iter()
			.map(|f| f.fixed_version.clone())
			.collect::<Vec<_>>(),
	)
	.bind(
		findings
			.iter()
			.map(|f| f.severity.clone())
			.collect::<Vec<_>>(),
	)
	.bind(findings.iter().map(|f| f.cvss).collect::<Vec<_>>())
	.bind(findings.iter().map(|f| f.epss).collect::<Vec<_>>())
	.bind(findings.iter().map(|f| f.kev).collect::<Vec<_>>())
	.bind(
		findings
			.iter()
			.map(|f| is_act_now(f.kev, f.epss, f.cvss))
			.collect::<Vec<_>>(),
	)
	.bind(findings.iter().map(|f| f.url.clone()).collect::<Vec<_>>())
	.execute(&mut *tx)
	.await
	.map(drop)
}

/// The gzipped SBOM of a scan in `team`, if it has one.
pub async fn sbom(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: ScanId,
) -> Result<Option<Vec<u8>>, sqlx::Error> {
	sqlx::query_scalar::<_, Option<Vec<u8>>>(
		"SELECT sbom FROM image_scans WHERE id = $1 AND team_id = $2",
	)
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await
	.map(Option::flatten)
}

pub async fn sbom_of(db: impl PgExecutor<'_>, id: ScanId) -> Result<Option<Vec<u8>>, sqlx::Error> {
	sqlx::query_scalar::<_, Option<Vec<u8>>>("SELECT sbom FROM image_scans WHERE id = $1")
		.bind(id)
		.fetch_optional(db)
		.await
		.map(Option::flatten)
}

/// The scan each resource of `kind` among `ids` shows now (its newest link).
pub async fn latest(
	db: impl PgExecutor<'_>,
	kind: ResourceKind,
	ids: &[i64],
) -> Result<HashMap<i64, Scan>, sqlx::Error> {
	#[derive(sqlx::FromRow)]
	struct Row {
		resource_id: i64,
		#[sqlx(flatten)]
		scan: Scan,
	}
	let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT DISTINCT ON (l.resource_id) l.resource_id, {SCAN_COLUMNS}
		FROM scan_links l JOIN image_scans s ON s.id = l.scan_id
		WHERE l.resource_kind = $1 AND l.resource_id = ANY($2)
		ORDER BY l.resource_id, l.created_at DESC, l.scan_id DESC"
	)))
	.bind(kind.as_str())
	.bind(ids)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(|r| (r.resource_id, r.scan)).collect())
}

/// The scans that deployments queued, by deployment.
pub async fn for_deployments(
	db: impl PgExecutor<'_>,
	ids: &[i64],
) -> Result<HashMap<i64, Scan>, sqlx::Error> {
	#[derive(sqlx::FromRow)]
	struct Row {
		deployment_id: i64,
		#[sqlx(flatten)]
		scan: Scan,
	}
	let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT DISTINCT ON (l.deployment_id) l.deployment_id, {SCAN_COLUMNS}
		FROM scan_links l JOIN image_scans s ON s.id = l.scan_id
		WHERE l.resource_kind = 'application' AND l.deployment_id = ANY($1)
		ORDER BY l.deployment_id, l.created_at DESC"
	)))
	.bind(ids)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| (r.deployment_id, r.scan))
		.collect())
}

/// Counts of each scan with the team's live ignores taken out.
pub async fn effective_counts(
	db: impl PgExecutor<'_>,
	team: TeamId,
	scans: &[ScanId],
) -> Result<HashMap<ScanId, Counts>, sqlx::Error> {
	let rows: Vec<(ScanId, String, bool, i64)> = sqlx::query_as(
		"SELECT f.scan_id, f.severity, f.act_now, COUNT(*) FROM scan_findings f
		WHERE f.scan_id = ANY($1)
			AND NOT EXISTS (
				SELECT 1 FROM scan_ignores i
				WHERE i.team_id = $2 AND i.vuln_id = f.vuln_id AND i.expires_at > NOW()
					AND (i.package IS NULL OR i.package = f.package))
		GROUP BY f.scan_id, f.severity, f.act_now",
	)
	.bind(scans.iter().map(|s| s.0).collect::<Vec<_>>())
	.bind(team)
	.fetch_all(db)
	.await?;
	let mut out: HashMap<ScanId, Counts> = HashMap::new();
	for (scan, severity, act_now, n) in rows {
		let counts = out.entry(scan).or_default();
		let severity = Severity::parse(&severity).unwrap_or(Severity::Unknown);
		for _ in 0..n {
			counts.add(severity, act_now);
		}
	}
	Ok(out)
}

/// The badge a scan earns once the team's ignores are applied.
#[must_use]
pub fn badge_of(scan: Option<&Scan>, effective: &HashMap<ScanId, Counts>) -> Badge {
	let Some(scan) = scan else {
		return Badge::NONE;
	};
	match scan.status {
		ScanStatus::Queued | ScanStatus::Scanning => Badge {
			state: BadgeState::Scanning,
			counts: Counts::default(),
		},
		ScanStatus::Failed => Badge {
			state: BadgeState::Failed,
			counts: Counts::default(),
		},
		ScanStatus::Succeeded => Badge::found(effective.get(&scan.id).copied().unwrap_or_default()),
	}
}

/// A scan's findings, newest risk first, with the ignore flag worked out for `now`.
pub async fn findings(
	db: impl PgExecutor<'_>,
	team: TeamId,
	scan: ScanId,
	filter: FindingFilter,
) -> Result<Vec<StoredFinding>, sqlx::Error> {
	sqlx::query_as(
		"SELECT f.vuln_id, f.package, f.version, f.fixed_version, f.severity, f.cvss, f.epss, f.kev,
			f.act_now, f.url,
			EXISTS (SELECT 1 FROM scan_ignores i
				WHERE i.team_id = $2 AND i.vuln_id = f.vuln_id AND i.expires_at > NOW()
					AND (i.package IS NULL OR i.package = f.package)) AS ignored
		FROM scan_findings f
		WHERE f.scan_id = $1
			AND ($3::text IS NULL OR f.severity = $3)
			AND (NOT $4 OR f.act_now)
			AND (NOT $5 OR f.fixed_version IS NOT NULL)
			AND ($6 OR NOT EXISTS (SELECT 1 FROM scan_ignores i
				WHERE i.team_id = $2 AND i.vuln_id = f.vuln_id AND i.expires_at > NOW()
					AND (i.package IS NULL OR i.package = f.package)))
		ORDER BY f.act_now DESC,
			CASE f.severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2
				WHEN 'low' THEN 1 ELSE 0 END DESC,
			f.cvss DESC NULLS LAST, f.package, f.vuln_id
		LIMIT 2000",
	)
	.bind(scan)
	.bind(team)
	.bind(filter.severity.map(Severity::as_str))
	.bind(filter.act_now)
	.bind(filter.fixable)
	.bind(filter.show_ignored)
	.fetch_all(db)
	.await
}

pub async fn add_ignore(
	db: impl PgExecutor<'_>,
	team: TeamId,
	user: UserId,
	vuln_id: &str,
	package: Option<&str>,
	reason: &str,
	expires_at: OffsetDateTime,
) -> Result<ScanIgnoreId, sqlx::Error> {
	let id = ScanIgnoreId::generate();
	sqlx::query(
		"INSERT INTO scan_ignores (id, team_id, vuln_id, package, reason, expires_at, created_by)
		VALUES ($1, $2, $3, $4, $5, $6, $7)",
	)
	.bind(id)
	.bind(team)
	.bind(vuln_id)
	.bind(package)
	.bind(reason)
	.bind(expires_at)
	.bind(user)
	.execute(db)
	.await?;
	Ok(id)
}

/// Deletes an ignore of `team`; false when there is none (or it is another team's).
pub async fn remove_ignore(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: ScanIgnoreId,
) -> Result<bool, sqlx::Error> {
	sqlx::query("DELETE FROM scan_ignores WHERE id = $1 AND team_id = $2")
		.bind(id)
		.bind(team)
		.execute(db)
		.await
		.map(|r| r.rows_affected() > 0)
}

/// The team's ignores, live ones first.
pub async fn ignores(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<StoredIgnore>, sqlx::Error> {
	sqlx::query_as(
		"SELECT i.id, i.vuln_id, i.package, i.reason, i.expires_at,
			u.name AS created_by_name, i.created_at
		FROM scan_ignores i LEFT JOIN users u ON u.id = i.created_by
		WHERE i.team_id = $1
		ORDER BY (i.expires_at > NOW()) DESC, i.expires_at DESC, i.id DESC",
	)
	.bind(team)
	.fetch_all(db)
	.await
}

/// Scans a daily rescan should refresh on `server`: the newest succeeded scan, with an
/// SBOM, of every resource that is running there now.
pub async fn running_scans(
	db: impl PgExecutor<'_>,
	server: ServerId,
) -> Result<Vec<ScanId>, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT DISTINCT s.id FROM scan_links l JOIN image_scans s ON s.id = l.scan_id
		WHERE s.server_id = $1 AND s.status = 'succeeded' AND s.sbom IS NOT NULL
			AND l.created_at = (SELECT MAX(l2.created_at) FROM scan_links l2
				WHERE l2.resource_kind = l.resource_kind AND l2.resource_id = l.resource_id)
			AND CASE l.resource_kind
				WHEN 'application' THEN EXISTS (SELECT 1 FROM deployments d
					WHERE d.id = l.deployment_id AND d.status = 'running')
				WHEN 'stack_service' THEN EXISTS (SELECT 1 FROM stack_services x
					WHERE x.id = l.resource_id AND x.status = 'running')
				WHEN 'database' THEN EXISTS (SELECT 1 FROM databases x
					WHERE x.id = l.resource_id AND x.status = 'running')
				WHEN 'model' THEN EXISTS (SELECT 1 FROM models x
					WHERE x.id = l.resource_id AND x.status = 'running')
				ELSE FALSE END
		ORDER BY s.id",
	)
	.bind(server)
	.fetch_all(db)
	.await
}
