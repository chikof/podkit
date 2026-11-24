//! Queueing scans. A deploy calls [`queue`] with what it deployed and goes on
//! without waiting: this only writes rows and wakes the job dispatcher.

use time::OffsetDateTime;

use super::{RESCAN_EVERY, ResourceKind, store};
use crate::ids::{DeploymentId, ScanId, ServerId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;

/// What a deploy hands over.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
	pub kind: ResourceKind,
	pub resource_id: i64,
	/// The image the resource now runs: a digest reference, or a local tag.
	pub image_ref: &'a str,
	/// The deployment that started it, for applications.
	pub deployment: Option<DeploymentId>,
}

/// Queues a scan of `target` on `server` and links it to the resource. Does
/// nothing when scanning is off for the server. It never fails the caller: a
/// scan is a courtesy, so a database error is logged and the deploy goes on.
pub async fn queue(state: &AppState, server: ServerId, target: Target<'_>) -> Option<ScanId> {
	match queue_inner(state, server, target, false).await {
		Ok(id) => id,
		Err(e) => {
			tracing::warn!(server = server.0, "could not queue an image scan: {e}");
			None
		}
	}
}

/// Like [`queue`], but skips the 24 h reuse: the "scan again" button.
pub async fn queue_fresh(
	state: &AppState,
	server: ServerId,
	target: Target<'_>,
) -> Result<Option<ScanId>, sqlx::Error> {
	queue_inner(state, server, target, true).await
}

async fn queue_inner(
	state: &AppState,
	server: ServerId,
	target: Target<'_>,
	force: bool,
) -> Result<Option<ScanId>, sqlx::Error> {
	let Some(settings) = store::server_settings(&state.db, server).await? else {
		return Ok(None);
	};
	if !settings.scan_enabled || target.image_ref.trim().is_empty() {
		return Ok(None);
	}
	let id =
		store::insert_queued(&state.db, settings.team_id, server, target.image_ref, force).await?;
	store::link(
		&state.db,
		target.kind,
		target.resource_id,
		id,
		target.deployment,
	)
	.await?;
	jobs::enqueue(
		state,
		JobKind::ImageScan,
		server,
		id.0,
		format!("scan:{server}"),
	)
	.await?;
	Ok(Some(id))
}

/// Queues the daily rescan of every server that is due: scanning on, something
/// running with a stored SBOM, and no rescan for 23 hours. Called by the
/// hourly maintenance tick; a server is never rescanned twice a day.
pub async fn queue_daily_rescans(state: &AppState) {
	if let Err(e) = queue_due_rescans(state, OffsetDateTime::now_utc()).await {
		tracing::warn!("could not queue the daily image rescans: {e}");
	}
}

pub(super) async fn queue_due_rescans(
	state: &AppState,
	now: OffsetDateTime,
) -> Result<usize, sqlx::Error> {
	let due: Vec<ServerId> = sqlx::query_scalar(
		"SELECT id FROM servers
		WHERE scan_enabled AND (scan_rescanned_at IS NULL OR scan_rescanned_at < $1)
			AND EXISTS (SELECT 1 FROM image_scans s WHERE s.server_id = servers.id
				AND s.status = 'succeeded' AND s.sbom IS NOT NULL)
		ORDER BY id",
	)
	.bind(now - RESCAN_EVERY)
	.fetch_all(&state.db)
	.await?;
	let mut queued = 0;
	for server in due {
		if jobs::active_exists(&state.db, JobKind::ImageRescan, server.0).await? {
			continue;
		}
		// Stamped now, not when it ends: a rescan that fails waits for tomorrow
		// instead of hammering a server that is down.
		sqlx::query("UPDATE servers SET scan_rescanned_at = $2 WHERE id = $1")
			.bind(server)
			.bind(now)
			.execute(&state.db)
			.await?;
		jobs::enqueue(
			state,
			JobKind::ImageRescan,
			server,
			server.0,
			format!("scan:{server}"),
		)
		.await?;
		queued += 1;
	}
	Ok(queued)
}
