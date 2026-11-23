//! The series beyond CPU and memory: HTTP, database, volume, GPU and model figures of a
//! resource, in `resource_extra_metrics`. Same shape as `resource_metrics` (see the module
//! docs of [`super`]): raw rows for 24 hours, 5-minute rollups for 7 days, at most
//! [`super::MAX_POINTS`] points per response.
//!
//! These queries are plain `sqlx::query_as` over fixed text, with every value bound.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{Kind, MAX_POINTS, Point, Range, downsample, mean, mean_i64};

/// One reading of the extra series. Every figure is optional: a resource has the HTTP ones only
/// when it is routed, the database ones only when it is a database, and so on. `at` is unix
/// seconds; rates are per second.
#[derive(Debug, Clone, Default, PartialEq, Serialize, sqlx::FromRow)]
pub struct ExtPoint {
	pub at: i64,
	pub req_rate: Option<f64>,
	pub r2xx_rate: Option<f64>,
	pub r3xx_rate: Option<f64>,
	pub r4xx_rate: Option<f64>,
	pub r5xx_rate: Option<f64>,
	pub p50_ms: Option<f64>,
	pub p95_ms: Option<f64>,
	pub bytes_in_rate: Option<f64>,
	pub bytes_out_rate: Option<f64>,
	pub db_conns: Option<i64>,
	pub db_size: Option<i64>,
	pub db_ops_rate: Option<f64>,
	pub volume_bytes: Option<i64>,
	pub vram_bytes: Option<i64>,
	pub tokens_rate: Option<f64>,
	pub model_req_rate: Option<f64>,
}

impl ExtPoint {
	/// Whether no figure is set.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		*self
			== Self {
				at: self.at,
				..Self::default()
			}
	}
}

/// The figures of one tick, by resource, filled in by each source in turn.
pub type ExtSamples = BTreeMap<(String, i64), ExtPoint>;

/// The columns of `resource_extra_metrics` after the key, in [`ExtPoint`] order.
const COLUMNS: &str = "req_rate, r2xx_rate, r3xx_rate, r4xx_rate, r5xx_rate, p50_ms, p95_ms, \
	bytes_in_rate, bytes_out_rate, db_conns, db_size, db_ops_rate, volume_bytes, vram_bytes, \
	tokens_rate, model_req_rate";

/// Stores one tick's figures taken at `at`, one row per resource with at least one figure.
///
/// # Errors
/// The database rejects a write.
pub async fn record(
	db: &PgPool,
	at: OffsetDateTime,
	samples: &ExtSamples,
) -> Result<(), sqlx::Error> {
	let mut tx = db.begin().await?;
	for ((kind, id), p) in samples.iter().filter(|(_, p)| !p.is_empty()) {
		sqlx::query(sqlx::AssertSqlSafe(format!(
			"INSERT INTO resource_extra_metrics (kind, resource_id, at, {COLUMNS})
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
			ON CONFLICT DO NOTHING"
		)))
		.bind(kind)
		.bind(id)
		.bind(at)
		.bind(p.req_rate)
		.bind(p.r2xx_rate)
		.bind(p.r3xx_rate)
		.bind(p.r4xx_rate)
		.bind(p.r5xx_rate)
		.bind(p.p50_ms)
		.bind(p.p95_ms)
		.bind(p.bytes_in_rate)
		.bind(p.bytes_out_rate)
		.bind(p.db_conns)
		.bind(p.db_size)
		.bind(p.db_ops_rate)
		.bind(p.volume_bytes)
		.bind(p.vram_bytes)
		.bind(p.tokens_rate)
		.bind(p.model_req_rate)
		.execute(&mut *tx)
		.await?;
	}
	tx.commit().await
}

/// Averages every finished 5-minute bucket of raw rows into a rollup row, once, like
/// [`super::rollup`]. A rate or gauge that has no reading in the bucket stays empty.
///
/// # Errors
/// The database rejects a write.
pub async fn rollup(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO resource_extra_metrics
			(kind, resource_id, at, rollup, req_rate, r2xx_rate, r3xx_rate, r4xx_rate, r5xx_rate,
			 p50_ms, p95_ms, bytes_in_rate, bytes_out_rate, db_conns, db_size, db_ops_rate,
			 volume_bytes, vram_bytes, tokens_rate, model_req_rate)
		SELECT kind, resource_id, date_bin('5 minutes', at, TIMESTAMPTZ '2000-01-01'), TRUE,
			AVG(req_rate), AVG(r2xx_rate), AVG(r3xx_rate), AVG(r4xx_rate), AVG(r5xx_rate),
			AVG(p50_ms), AVG(p95_ms), AVG(bytes_in_rate), AVG(bytes_out_rate),
			AVG(db_conns)::BIGINT, AVG(db_size)::BIGINT, AVG(db_ops_rate),
			AVG(volume_bytes)::BIGINT, AVG(vram_bytes)::BIGINT, AVG(tokens_rate),
			AVG(model_req_rate)
		FROM resource_extra_metrics
		WHERE NOT rollup AND at < date_bin('5 minutes', $1::timestamptz, TIMESTAMPTZ '2000-01-01')
		GROUP BY kind, resource_id, date_bin('5 minutes', at, TIMESTAMPTZ '2000-01-01')
		ON CONFLICT DO NOTHING",
	)
	.bind(now)
	.execute(db)
	.await?;
	Ok(())
}

/// Drops raw rows older than [`super::RAW_RETENTION`] and rollups older than
/// [`super::ROLLUP_RETENTION`].
///
/// # Errors
/// The database rejects a write.
pub async fn prune(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	sqlx::query(
		"DELETE FROM resource_extra_metrics WHERE (NOT rollup AND at < $1) OR (rollup AND at < $2)",
	)
	.bind(now - super::RAW_RETENTION)
	.bind(now - super::ROLLUP_RETENTION)
	.execute(db)
	.await?;
	Ok(())
}

/// A resource's extra series over `range` ending at `now`, oldest first, at most
/// [`MAX_POINTS`] long. Rows with no figure at all are not stored, so a resource that never
/// had any of these figures answers an empty list.
///
/// # Errors
/// The database rejects the read.
pub async fn series(
	db: &PgPool,
	kind: Kind,
	id: i64,
	range: Range,
	now: OffsetDateTime,
) -> Result<Vec<ExtPoint>, sqlx::Error> {
	let from = now - range.duration();
	let points = sqlx::query_as::<_, ExtPoint>(sqlx::AssertSqlSafe(format!(
		"SELECT EXTRACT(EPOCH FROM at)::BIGINT AS at, {COLUMNS}
		FROM resource_extra_metrics
		WHERE kind = $1 AND resource_id = $2 AND at >= $3 AND at <= $4
			AND (
				(rollup AND NOT $5)
				OR (NOT rollup AND ($5 OR at >= COALESCE(
					(SELECT MAX(at) + INTERVAL '5 minutes' FROM resource_extra_metrics
					 WHERE kind = $1 AND resource_id = $2 AND rollup), $3)))
			)
		ORDER BY at"
	)))
	.bind(kind.as_str())
	.bind(id)
	.bind(from)
	.bind(now)
	.bind(range.raw_only())
	.fetch_all(db)
	.await?;
	Ok(downsample(
		points,
		from.unix_timestamp(),
		now.unix_timestamp(),
		MAX_POINTS,
	))
}

impl Point for ExtPoint {
	fn at(&self) -> i64 {
		self.at
	}

	/// Rates and gauges average over the run; a figure absent from every reading stays absent.
	fn merge(run: &[Self]) -> Self {
		let last = &run[run.len() - 1];
		let f = |get: fn(&Self) -> Option<f64>| mean(run.iter().map(get));
		let i = |get: fn(&Self) -> Option<i64>| mean_i64(run.iter().map(get));
		Self {
			at: last.at,
			req_rate: f(|p| p.req_rate),
			r2xx_rate: f(|p| p.r2xx_rate),
			r3xx_rate: f(|p| p.r3xx_rate),
			r4xx_rate: f(|p| p.r4xx_rate),
			r5xx_rate: f(|p| p.r5xx_rate),
			p50_ms: f(|p| p.p50_ms),
			p95_ms: f(|p| p.p95_ms),
			bytes_in_rate: f(|p| p.bytes_in_rate),
			bytes_out_rate: f(|p| p.bytes_out_rate),
			db_conns: i(|p| p.db_conns),
			db_size: i(|p| p.db_size),
			db_ops_rate: f(|p| p.db_ops_rate),
			volume_bytes: i(|p| p.volume_bytes),
			vram_bytes: i(|p| p.vram_bytes),
			tokens_rate: f(|p| p.tokens_rate),
			model_req_rate: f(|p| p.model_req_rate),
		}
	}
}
