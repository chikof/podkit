//! Periodic housekeeping that runs for the life of the process.

use std::time::Duration;

use crate::auth::session;
use crate::ids::JobId;
use crate::state::AppState;

const RETENTION: Duration = Duration::from_hours(24 * 30);

pub fn spawn(state: AppState) {
	tokio::spawn(async move {
		let mut interval = tokio::time::interval(Duration::from_hours(1));
		loop {
			interval.tick().await;
			if let Err(e) = session::purge_expired(&state.db).await {
				tracing::warn!("failed to purge expired sessions: {e}");
			}
			if let Err(e) = crate::auth::flow::purge_expired(&state.db).await {
				tracing::warn!("failed to purge expired sign-in steps: {e}");
			}
			if let Err(e) = purge_jobs(&state).await {
				tracing::warn!("failed to purge old jobs: {e}");
			}
			match crate::audit::service::purge_expired(&state.db).await {
				Ok(0) => {}
				Ok(n) => tracing::info!("purged {n} audit events past their retention"),
				Err(e) => tracing::warn!("failed to purge audit events: {e}"),
			}
			purge_orphan_logs(&state).await;
			crate::metrics::maintain(&state.db).await;
			crate::scanning::queue_daily_rescans(&state).await;
			crate::notifications::maintain(&state.db).await;
			crate::cron::scheduler::purge(&state).await;
			crate::uptime::maintain(&state.db).await;
			crate::previews::service::sweep(&state).await;
		}
	});
}

/// Deletes jobs finished more than 30 days ago together with their log files.
async fn purge_jobs(state: &AppState) -> Result<(), sqlx::Error> {
	let ids = sqlx::query_scalar::<_, JobId>(
		"DELETE FROM jobs WHERE finished_at < NOW() - INTERVAL '30 days' RETURNING id",
	)
	.fetch_all(&state.db)
	.await?;
	for id in ids {
		remove_log(state.logs.path(id)).await;
	}
	Ok(())
}

/// Deletes log files older than 30 days whose job row no longer exists.
async fn purge_orphan_logs(state: &AppState) {
	let dir = state.config.data_dir.join("logs").join("jobs");
	let mut entries = match tokio::fs::read_dir(&dir).await {
		Ok(entries) => entries,
		Err(e) => {
			tracing::warn!("cannot scan job logs in {}: {e}", dir.display());
			return;
		}
	};
	loop {
		let entry = match entries.next_entry().await {
			Ok(Some(entry)) => entry,
			Ok(None) => return,
			Err(e) => {
				tracing::warn!("cannot scan job logs in {}: {e}", dir.display());
				return;
			}
		};
		let path = entry.path();
		let Some(id) = path
			.file_name()
			.and_then(|n| n.to_str())
			.and_then(|n| n.strip_suffix(".ndjson"))
			.and_then(|n| n.parse::<i64>().ok())
		else {
			continue;
		};
		let old = entry
			.metadata()
			.await
			.and_then(|m| m.modified())
			.is_ok_and(|t| t.elapsed().is_ok_and(|age| age > RETENTION));
		if !old {
			continue;
		}
		match sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM jobs WHERE id = $1)")
			.bind(id)
			.fetch_one(&state.db)
			.await
		{
			Ok(false) => remove_log(path).await,
			Ok(true) => {}
			Err(e) => tracing::warn!("cannot check job {id} for log cleanup: {e}"),
		}
	}
}

async fn remove_log(path: std::path::PathBuf) {
	if let Err(e) = tokio::fs::remove_file(&path).await
		&& e.kind() != std::io::ErrorKind::NotFound
	{
		tracing::warn!("cannot remove job log {}: {e}", path.display());
	}
}
