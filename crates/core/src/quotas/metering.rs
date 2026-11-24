//! Hourly usage records, computed from what podkit already measures.
//!
//! Once an hour (from the maintenance loop) [`run`] looks at the last 24 complete hours and
//! writes a `usage_records` row for every team-hour that has none yet. An hour already written
//! is never touched again, so a billing provider that was told about it keeps hearing the same
//! numbers; an hour with no usage gets no row.
//!
//! Where each figure comes from:
//!
//! - **CPU-seconds** and **memory GB-hours**: `resource_metrics` (the sampler's per-container
//!   CPU share of the host and memory). Samples are averaged per 5-minute slot, and each slot a
//!   resource has a sample in counts as 5 minutes at that average: CPU share × the host's
//!   threads × 300 s, memory × 1/12 h. A server that was down for part of an hour therefore
//!   costs the team nothing for that part.
//! - **Disk GB-hours**: the volume bytes the agents measured (`resource_extra_metrics`), the
//!   same way. Images are shared per server and not counted.
//! - **GPU-hours**: a slot in which a resource held GPU memory counts as 1 GPU for 5 minutes.
//! - **Egress bytes**: the proxy's outgoing byte rate for the resource (the HTTP series of the
//!   charts card) × 300 s per slot. Resources without HTTP traffic have none.
//! - **Model tokens**: `model_usage`, which the status sync fills from the gateway.
//! - **Build minutes**: the time git-built applications spent between a deployment's start and
//!   finish, counted in the hour the deployment started.
//!
//! The samples live 24 hours (raw) and 7 days (rollups), which is why the pass looks back 24
//! hours: a server that was off for a day loses nothing, a longer outage loses the oldest hours.

use time::{Duration, OffsetDateTime};

use crate::state::AppState;

/// How many hours back a pass looks.
const LOOKBACK_HOURS: i64 = 24;

/// One team-hour as written.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Record {
	/// The team's id.
	pub team_id: i64,
	/// Start of the hour.
	pub hour: OffsetDateTime,
	/// CPU time.
	pub cpu_seconds: f64,
	/// Memory held, GB-hours.
	pub memory_gb_hours: f64,
	/// Volume data held, GB-hours.
	pub disk_gb_hours: f64,
	/// GPU time.
	pub gpu_hours: f64,
	/// Bytes out through the proxy.
	pub egress_bytes: i64,
	/// Tokens the gateway served.
	pub model_tokens: i64,
	/// Build time.
	pub build_minutes: f64,
}

/// The first instant of the hour `at` is in.
fn hour_floor(at: OffsetDateTime) -> OffsetDateTime {
	let secs = at.unix_timestamp();
	OffsetDateTime::from_unix_timestamp(secs - secs.rem_euclid(3600)).unwrap_or(at)
}

const SQL: &str = "
WITH res AS (
	SELECT 'application'::text AS kind, a.id AS rid, p.team_id,
		COALESCE((s.hardware #>> '{cpu,threads}')::int, 1) AS threads
		FROM applications a JOIN projects p ON p.id = a.project_id JOIN servers s ON s.id = a.server_id
	UNION ALL
	SELECT 'database', d.id, p.team_id, COALESCE((s.hardware #>> '{cpu,threads}')::int, 1)
		FROM databases d JOIN projects p ON p.id = d.project_id JOIN servers s ON s.id = d.server_id
	UNION ALL
	SELECT 'model', m.id, p.team_id, COALESCE((s.hardware #>> '{cpu,threads}')::int, 1)
		FROM models m JOIN projects p ON p.id = m.project_id JOIN servers s ON s.id = m.server_id
	UNION ALL
	SELECT 'stack', st.id, p.team_id, COALESCE((s.hardware #>> '{cpu,threads}')::int, 1)
		FROM stacks st JOIN projects p ON p.id = st.project_id JOIN servers s ON s.id = st.server_id
),
cpu_slots AS (
	SELECT r.team_id, r.threads,
		date_bin('5 minutes', m.at, TIMESTAMPTZ '2000-01-01 00:00:00+00') AS slot,
		AVG(m.cpu) AS cpu, AVG(m.mem::float8) AS mem
	FROM resource_metrics m JOIN res r ON r.kind = m.kind AND r.rid = m.resource_id
	WHERE m.at >= $1 AND m.at < $2
	GROUP BY r.team_id, r.kind, r.rid, r.threads, slot
),
ext_slots AS (
	SELECT r.team_id,
		date_bin('5 minutes', e.at, TIMESTAMPTZ '2000-01-01 00:00:00+00') AS slot,
		AVG(e.volume_bytes::float8) AS vol, AVG(e.vram_bytes::float8) AS vram,
		AVG(e.bytes_out_rate) AS out_rate
	FROM resource_extra_metrics e JOIN res r ON r.kind = e.kind AND r.rid = e.resource_id
	WHERE e.at >= $1 AND e.at < $2
	GROUP BY r.team_id, r.kind, r.rid, slot
),
parts AS (
	SELECT team_id, date_trunc('hour', slot, 'UTC') AS hour,
		COALESCE(cpu, 0) / 100.0 * threads * 300.0 AS cpu_seconds,
		COALESCE(mem, 0) / 1073741824.0 / 12.0 AS memory_gb_hours,
		0::float8 AS disk_gb_hours, 0::float8 AS gpu_hours, 0::float8 AS egress_bytes,
		0::float8 AS model_tokens, 0::float8 AS build_minutes
	FROM cpu_slots
	UNION ALL
	SELECT team_id, date_trunc('hour', slot, 'UTC'), 0, 0,
		COALESCE(vol, 0) / 1073741824.0 / 12.0,
		CASE WHEN COALESCE(vram, 0) > 0 THEN 1.0 / 12.0 ELSE 0 END,
		COALESCE(out_rate, 0) * 300.0, 0, 0
	FROM ext_slots
	UNION ALL
	SELECT team_id, hour, 0, 0, 0, 0, 0, (input_tokens + output_tokens)::float8, 0
	FROM model_usage WHERE team_id IS NOT NULL AND hour >= $1 AND hour < $2
	UNION ALL
	SELECT p.team_id, date_trunc('hour', d.started_at, 'UTC'), 0, 0, 0, 0, 0, 0,
		EXTRACT(EPOCH FROM (d.finished_at - d.started_at))::float8 / 60.0
	FROM deployments d
		JOIN applications a ON a.id = d.application_id
		JOIN projects p ON p.id = a.project_id
	WHERE a.source_kind = 'git' AND d.finished_at IS NOT NULL
		AND d.started_at >= $1 AND d.started_at < $2
),
total AS (
	SELECT x.team_id, x.hour,
		SUM(cpu_seconds) AS cpu_seconds, SUM(memory_gb_hours) AS memory_gb_hours,
		SUM(disk_gb_hours) AS disk_gb_hours, SUM(gpu_hours) AS gpu_hours,
		ROUND(SUM(egress_bytes))::BIGINT AS egress_bytes,
		ROUND(SUM(model_tokens))::BIGINT AS model_tokens, SUM(build_minutes) AS build_minutes
	FROM parts x JOIN teams t ON t.id = x.team_id
	GROUP BY x.team_id, x.hour
)
INSERT INTO usage_records (team_id, hour, cpu_seconds, memory_gb_hours, disk_gb_hours, gpu_hours,
	egress_bytes, model_tokens, build_minutes)
SELECT team_id, hour, cpu_seconds, memory_gb_hours, disk_gb_hours, gpu_hours,
	egress_bytes, model_tokens, build_minutes
FROM total
WHERE cpu_seconds > 0 OR memory_gb_hours > 0 OR disk_gb_hours > 0 OR gpu_hours > 0
	OR egress_bytes > 0 OR model_tokens > 0 OR build_minutes > 0
ON CONFLICT (team_id, hour) DO NOTHING
RETURNING team_id, hour, cpu_seconds, memory_gb_hours, disk_gb_hours, gpu_hours,
	egress_bytes, model_tokens, build_minutes";

/// Writes the usage records still missing for the last 24 complete hours before `now` and
/// returns the new ones.
///
/// # Errors
/// Database failures only.
pub async fn compute(db: &sqlx::PgPool, now: OffsetDateTime) -> Result<Vec<Record>, sqlx::Error> {
	let until = hour_floor(now);
	let since = until - Duration::hours(LOOKBACK_HOURS);
	sqlx::query_as::<_, Record>(SQL)
		.bind(since)
		.bind(until)
		.fetch_all(db)
		.await
}

/// The hourly pass: meter, hand the new records to the billing provider, then see whether
/// any team crossed a limit. Never fails the maintenance loop.
pub async fn run(s: &AppState) {
	match compute(&s.db, OffsetDateTime::now_utc()).await {
		Ok(records) => {
			if !records.is_empty() {
				tracing::info!(records = records.len(), "metered usage");
				crate::billing::usage_metered(s, &records).await;
			}
		}
		Err(e) => tracing::warn!("usage could not be metered: {e}"),
	}
	super::service::evaluate_all(s).await;
	super::service::purge(&s.db).await;
}
