//! Time series of the servers and of the containers on them.
//!
//! Every `metrics_interval_secs` the sampler asks each active server's agent
//! for `/v1/stats` and stores one `server_metrics` row plus one
//! `resource_metrics` row per application, database, model or stack that has a
//! running container. Raw rows live for 24 hours; the maintenance sweep rolls
//! them up into 5-minute averages, which live for 7 days. The API returns at
//! most [`MAX_POINTS`] points per series, averaged down on the server.
//!
//! Sampling is best effort. It never marks a server unreachable: only the
//! status sync's health check decides that.

mod routes;
pub mod sampler;
pub mod service;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use proto::{ContainerStats, Stats};
use serde::Serialize;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

pub use routes::{application_router, database_router, model_router, router, stack_router};

use crate::ids::ServerId;

/// Most points one response carries.
pub const MAX_POINTS: usize = 300;
/// Raw samples are dropped after this long.
pub const RAW_RETENTION: Duration = Duration::hours(24);
/// Rollups are dropped after this long.
pub const ROLLUP_RETENTION: Duration = Duration::days(7);

/// How far back a request looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
	/// The last hour.
	Hour,
	/// The last 24 hours.
	Day,
	/// The last 7 days.
	Week,
}

impl Range {
	/// `1h`, `24h` or `7d`.
	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"1h" => Some(Self::Hour),
			"24h" => Some(Self::Day),
			"7d" => Some(Self::Week),
			_ => None,
		}
	}

	/// Length of the window.
	#[must_use]
	pub fn duration(self) -> Duration {
		match self {
			Self::Hour => Duration::hours(1),
			Self::Day => Duration::hours(24),
			Self::Week => Duration::days(7),
		}
	}

	/// Whether raw rows cover the whole window; a week needs the rollups.
	fn raw_only(self) -> bool {
		self != Self::Week
	}
}

/// One reading of a server. `at` is unix seconds. Counters (`net_*`) are bytes
/// since boot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerPoint {
	pub at: i64,
	pub cpu: Option<f64>,
	pub mem_used: i64,
	pub mem_total: i64,
	pub disk_used: i64,
	pub disk_total: i64,
	pub net_rx: i64,
	pub net_tx: i64,
	pub gpu_util: Option<f64>,
	pub vram_used: Option<i64>,
}

/// One reading of an application, database, model or stack. `at` is unix seconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourcePoint {
	pub at: i64,
	pub cpu: Option<f64>,
	pub mem: i64,
}

/// What a resource kind is called in `resource_metrics.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	Application,
	Database,
	Model,
	Stack,
}

impl Kind {
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Model => "model",
			Self::Stack => "stack",
		}
	}
}

/// The containers of one resource, summed.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSample {
	pub kind: String,
	pub id: i64,
	pub cpu: Option<f64>,
	pub mem: u64,
}

/// Sums container figures per resource. A resource's CPU is `None` only when
/// none of its containers has one yet.
#[must_use]
pub fn aggregate(containers: &[ContainerStats]) -> Vec<ResourceSample> {
	let mut by_resource: BTreeMap<(&str, i64), (Option<f64>, u64)> = BTreeMap::new();
	for c in containers {
		let entry = by_resource
			.entry((c.resource.kind.as_str(), c.resource.id))
			.or_default();
		entry.0 = match (entry.0, c.cpu_percent) {
			(Some(a), Some(b)) => Some(a + b),
			(a, b) => a.or(b),
		};
		entry.1 = entry.1.saturating_add(c.mem_used);
	}
	by_resource
		.into_iter()
		.map(|((kind, id), (cpu, mem))| ResourceSample {
			kind: kind.to_string(),
			id,
			cpu,
			mem,
		})
		.collect()
}

fn to_i64(n: u64) -> i64 {
	i64::try_from(n).unwrap_or(i64::MAX)
}

/// Stores one sample of `server` taken at `at`: the host row and a row per
/// resource, in one transaction.
///
/// # Errors
/// The database rejects a write.
pub async fn record(
	db: &PgPool,
	server: ServerId,
	at: OffsetDateTime,
	stats: &Stats,
) -> Result<(), sqlx::Error> {
	let host = &stats.host;
	// The first disk is the one under the Podman storage.
	let (disk_used, disk_total) = host
		.disks
		.first()
		.map_or((0, 0), |d| (to_i64(d.used), to_i64(d.total)));
	let mut tx = db.begin().await?;
	sqlx::query!(
		r#"
			INSERT INTO server_metrics
				(server_id, at, cpu, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx,
				 gpu_util, vram_used)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
			ON CONFLICT DO NOTHING
		"#,
		server as ServerId,
		at,
		host.cpu_percent,
		to_i64(host.mem_used),
		to_i64(host.mem_total),
		disk_used,
		disk_total,
		to_i64(host.net_rx),
		to_i64(host.net_tx),
		host.gpu_util,
		host.vram_used.map(to_i64),
	)
	.execute(&mut *tx)
	.await?;
	for sample in aggregate(&stats.containers) {
		sqlx::query!(
			r#"
				INSERT INTO resource_metrics (kind, resource_id, at, cpu, mem)
				VALUES ($1, $2, $3, $4, $5)
				ON CONFLICT DO NOTHING
			"#,
			sample.kind,
			sample.id,
			at,
			sample.cpu,
			to_i64(sample.mem),
		)
		.execute(&mut *tx)
		.await?;
	}
	tx.commit().await
}

/// Averages every finished 5-minute bucket of raw rows into a rollup row.
/// A bucket is rolled up once and then left alone, so the raw rows can age
/// out without the rollup shrinking. Counters and totals keep their largest
/// value.
///
/// # Errors
/// The database rejects a write.
pub async fn rollup(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO server_metrics
				(server_id, at, rollup, cpu, mem_used, mem_total, disk_used, disk_total, net_rx,
				 net_tx, gpu_util, vram_used)
			SELECT server_id, date_bin('5 minutes', at, TIMESTAMPTZ '2000-01-01'), TRUE,
				AVG(cpu), AVG(mem_used)::BIGINT, MAX(mem_total), AVG(disk_used)::BIGINT,
				MAX(disk_total), MAX(net_rx), MAX(net_tx), AVG(gpu_util), AVG(vram_used)::BIGINT
			FROM server_metrics
			WHERE NOT rollup AND at < date_bin('5 minutes', $1::timestamptz, TIMESTAMPTZ '2000-01-01')
			GROUP BY server_id, date_bin('5 minutes', at, TIMESTAMPTZ '2000-01-01')
			ON CONFLICT DO NOTHING
		"#,
		now
	)
	.execute(db)
	.await?;
	sqlx::query!(
		r#"
			INSERT INTO resource_metrics (kind, resource_id, at, rollup, cpu, mem)
			SELECT kind, resource_id, date_bin('5 minutes', at, TIMESTAMPTZ '2000-01-01'), TRUE,
				AVG(cpu), AVG(mem)::BIGINT
			FROM resource_metrics
			WHERE NOT rollup AND at < date_bin('5 minutes', $1::timestamptz, TIMESTAMPTZ '2000-01-01')
			GROUP BY kind, resource_id, date_bin('5 minutes', at, TIMESTAMPTZ '2000-01-01')
			ON CONFLICT DO NOTHING
		"#,
		now
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Drops raw rows older than 24 hours and rollups older than 7 days.
///
/// # Errors
/// The database rejects a write.
pub async fn prune(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	let raw = now - RAW_RETENTION;
	let old = now - ROLLUP_RETENTION;
	sqlx::query!(
		"DELETE FROM server_metrics WHERE (NOT rollup AND at < $1) OR (rollup AND at < $2)",
		raw,
		old
	)
	.execute(db)
	.await?;
	sqlx::query!(
		"DELETE FROM resource_metrics WHERE (NOT rollup AND at < $1) OR (rollup AND at < $2)",
		raw,
		old
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Rolls up, then prunes. Failures are logged; the next sweep tries again.
pub async fn maintain(db: &PgPool) {
	let now = OffsetDateTime::now_utc();
	if let Err(e) = rollup(db, now).await {
		tracing::warn!("failed to roll up metrics: {e}");
	}
	if let Err(e) = prune(db, now).await {
		tracing::warn!("failed to prune metrics: {e}");
	}
}

/// A server's series over `range` ending at `now`, oldest first, at most
/// [`MAX_POINTS`] long.
///
/// # Errors
/// The database rejects the read.
pub async fn server_series(
	db: &PgPool,
	server: ServerId,
	range: Range,
	now: OffsetDateTime,
) -> Result<Vec<ServerPoint>, sqlx::Error> {
	let from = now - range.duration();
	// Up to a week: the rollups, then the raw rows no rollup covers yet.
	let points = sqlx::query_as!(
		ServerPoint,
		r#"
			SELECT EXTRACT(EPOCH FROM at)::BIGINT AS "at!", cpu, mem_used, mem_total, disk_used,
				disk_total, net_rx, net_tx, gpu_util, vram_used
			FROM server_metrics
			WHERE server_id = $1 AND at >= $2 AND at <= $3
				AND (
					(rollup AND NOT $4)
					OR (NOT rollup AND ($4 OR at >= COALESCE(
						(SELECT MAX(at) + INTERVAL '5 minutes' FROM server_metrics
						 WHERE server_id = $1 AND rollup), $2)))
				)
			ORDER BY at
		"#,
		server as ServerId,
		from,
		now,
		range.raw_only(),
	)
	.fetch_all(db)
	.await?;
	Ok(downsample(
		points,
		from.unix_timestamp(),
		now.unix_timestamp(),
		MAX_POINTS,
	))
}

/// A resource's series, see [`server_series`].
///
/// # Errors
/// The database rejects the read.
pub async fn resource_series(
	db: &PgPool,
	kind: Kind,
	id: i64,
	range: Range,
	now: OffsetDateTime,
) -> Result<Vec<ResourcePoint>, sqlx::Error> {
	let from = now - range.duration();
	let points = sqlx::query_as!(
		ResourcePoint,
		r#"
			SELECT EXTRACT(EPOCH FROM at)::BIGINT AS "at!", cpu, mem
			FROM resource_metrics
			WHERE kind = $1 AND resource_id = $2 AND at >= $3 AND at <= $4
				AND (
					(rollup AND NOT $5)
					OR (NOT rollup AND ($5 OR at >= COALESCE(
						(SELECT MAX(at) + INTERVAL '5 minutes' FROM resource_metrics
						 WHERE kind = $1 AND resource_id = $2 AND rollup), $3)))
				)
			ORDER BY at
		"#,
		kind.as_str(),
		id,
		from,
		now,
		range.raw_only(),
	)
	.fetch_all(db)
	.await?;
	Ok(downsample(
		points,
		from.unix_timestamp(),
		now.unix_timestamp(),
		MAX_POINTS,
	))
}

/// A point that can be averaged with its neighbours.
pub trait Point: Sized {
	/// Unix seconds.
	fn at(&self) -> i64;
	/// One point for a non-empty run of points, sorted by time.
	fn merge(run: &[Self]) -> Self;
}

/// Averages `points` (sorted by time, inside `from..=to`) into at most `max`
/// equal time buckets. Fewer points than `max` pass through untouched.
#[must_use]
pub fn downsample<P: Point>(points: Vec<P>, from: i64, to: i64, max: usize) -> Vec<P> {
	if max == 0 || points.len() <= max {
		return points;
	}
	let max_i = i64::try_from(max).unwrap_or(i64::MAX);
	let width = ((to - from).max(1) + max_i - 1) / max_i;
	let bucket = |p: &P| ((p.at() - from).max(0) / width).min(max_i - 1);
	let mut out = Vec::with_capacity(max);
	let mut start = 0;
	while start < points.len() {
		let b = bucket(&points[start]);
		let mut end = start + 1;
		while end < points.len() && bucket(&points[end]) == b {
			end += 1;
		}
		out.push(P::merge(&points[start..end]));
		start = end;
	}
	out
}

fn mean(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
	let (sum, n) = values
		.flatten()
		.fold((0.0, 0u32), |(s, n), v| (s + v, n + 1));
	(n > 0).then(|| sum / f64::from(n))
}

#[allow(clippy::cast_possible_truncation)]
fn mean_i64(values: impl Iterator<Item = Option<i64>>) -> Option<i64> {
	#[allow(clippy::cast_precision_loss)]
	mean(values.map(|v| v.map(|v| v as f64))).map(|m| m.round() as i64)
}

impl Point for ServerPoint {
	fn at(&self) -> i64 {
		self.at
	}

	/// Gauges average; counters and totals take the newest value.
	fn merge(run: &[Self]) -> Self {
		let last = &run[run.len() - 1];
		Self {
			at: last.at,
			cpu: mean(run.iter().map(|p| p.cpu)),
			mem_used: mean_i64(run.iter().map(|p| Some(p.mem_used))).unwrap_or(last.mem_used),
			mem_total: last.mem_total,
			disk_used: mean_i64(run.iter().map(|p| Some(p.disk_used))).unwrap_or(last.disk_used),
			disk_total: last.disk_total,
			net_rx: last.net_rx,
			net_tx: last.net_tx,
			gpu_util: mean(run.iter().map(|p| p.gpu_util)),
			vram_used: mean_i64(run.iter().map(|p| p.vram_used)),
		}
	}
}

impl Point for ResourcePoint {
	fn at(&self) -> i64 {
		self.at
	}

	fn merge(run: &[Self]) -> Self {
		let last = &run[run.len() - 1];
		Self {
			at: last.at,
			cpu: mean(run.iter().map(|p| p.cpu)),
			mem: mean_i64(run.iter().map(|p| Some(p.mem))).unwrap_or(last.mem),
		}
	}
}
