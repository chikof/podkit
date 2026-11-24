//! Markers for the charts: what Podman said happened to a resource's containers
//! (`container_events`) and when an application was deployed (`deployments`).
//!
//! Events come from the sampler, which asks each agent for the window since its last question
//! and stores what the agent returns; nothing here subscribes to anything.

use proto::ContainerEvent;
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{Kind, Range};

/// Most markers one response carries, so a crash loop does not drown a chart.
const MAX_MARKERS: i64 = 100;

/// A restart, OOM kill or crash. `at` is unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct EventMarker {
	pub at: i64,
	/// `exit`, `oom` or `restart`.
	pub event: String,
	pub exit_code: Option<i32>,
	pub container: String,
}

/// One deployment of an application. `at` is when it started, in unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct DeployMarker {
	pub at: i64,
	pub id: i64,
	pub status: String,
	/// The commit, when the deployment built one.
	pub commit: Option<String>,
}

/// Stores the events one agent reported. A repeated event (the window of two questions
/// overlapping by a second) is ignored.
///
/// # Errors
/// The database rejects a write.
pub async fn record(db: &PgPool, events: &[ContainerEvent]) -> Result<(), sqlx::Error> {
	for e in events {
		let Some(at) = OffsetDateTime::from_unix_timestamp(e.at).ok() else {
			continue;
		};
		sqlx::query(
			"INSERT INTO container_events (kind, resource_id, at, event, container, exit_code)
			VALUES ($1, $2, $3, $4, $5, $6)
			ON CONFLICT DO NOTHING",
		)
		.bind(&e.resource.kind)
		.bind(e.resource.id)
		.bind(at)
		.bind(e.kind.as_str())
		.bind(&e.container)
		.bind(e.exit_code.and_then(|c| i32::try_from(c).ok()))
		.execute(db)
		.await?;
	}
	Ok(())
}

/// Drops events older than [`super::ROLLUP_RETENTION`].
///
/// # Errors
/// The database rejects a write.
pub async fn prune(db: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM container_events WHERE at < $1")
		.bind(now - super::ROLLUP_RETENTION)
		.execute(db)
		.await?;
	Ok(())
}

/// The events of a resource over `range` ending at `now`, oldest first. When more than
/// [`MAX_MARKERS`] happened, the newest ones.
///
/// # Errors
/// The database rejects the read.
pub async fn markers(
	db: &PgPool,
	kind: Kind,
	id: i64,
	range: Range,
	now: OffsetDateTime,
) -> Result<Vec<EventMarker>, sqlx::Error> {
	let mut rows = sqlx::query_as::<_, EventMarker>(
		"SELECT EXTRACT(EPOCH FROM at)::BIGINT AS at, event, exit_code, container
		FROM container_events
		WHERE kind = $1 AND resource_id = $2 AND at >= $3 AND at <= $4
		ORDER BY at DESC LIMIT $5",
	)
	.bind(kind.as_str())
	.bind(id)
	.bind(now - range.duration())
	.bind(now)
	.bind(MAX_MARKERS)
	.fetch_all(db)
	.await?;
	rows.reverse();
	Ok(rows)
}

/// The deployments of an application that started over `range` ending at `now`, oldest
/// first, newest [`MAX_MARKERS`] at most. A queued deployment has not started and is left
/// out.
///
/// # Errors
/// The database rejects the read.
pub async fn deploy_markers(
	db: &PgPool,
	application: i64,
	range: Range,
	now: OffsetDateTime,
) -> Result<Vec<DeployMarker>, sqlx::Error> {
	let mut rows = sqlx::query_as::<_, DeployMarker>(
		"SELECT EXTRACT(EPOCH FROM COALESCE(started_at, created_at))::BIGINT AS at, id, status,
			commit_sha AS commit
		FROM deployments
		WHERE application_id = $1 AND status <> 'queued'
			AND COALESCE(started_at, created_at) >= $2 AND COALESCE(started_at, created_at) <= $3
		ORDER BY COALESCE(started_at, created_at) DESC LIMIT $4",
	)
	.bind(application)
	.bind(now - range.duration())
	.bind(now)
	.bind(MAX_MARKERS)
	.fetch_all(db)
	.await?;
	rows.reverse();
	Ok(rows)
}
