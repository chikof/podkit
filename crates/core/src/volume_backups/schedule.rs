//! Scheduled volume backups: queues a backup for every policy whose schedule is due.

use std::time::Duration;

use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::{POLICY_COLUMNS, Policy, busy, policy_by_id, queue_backup, subject};
use crate::backups::next_run;
use crate::state::AppState;

const INTERVAL: Duration = Duration::from_mins(1);

/// Queues the due backups once a minute until `shutdown`.
pub async fn run(state: AppState, shutdown: CancellationToken) {
	loop {
		sweep(&state).await;
		tokio::select! {
			() = shutdown.cancelled() => return,
			() = tokio::time::sleep(INTERVAL) => {}
		}
	}
}

pub(super) async fn sweep(state: &AppState) {
	let due = match due(state).await {
		Ok(due) => due,
		Err(e) => {
			tracing::warn!("volume backup scheduler could not list due volumes: {e}");
			return;
		}
	};
	for policy in due {
		queue_one(state, policy).await;
	}
}

async fn due(state: &AppState) -> Result<Vec<Policy>, sqlx::Error> {
	sqlx::query_as::<_, Policy>(sqlx::AssertSqlSafe(format!(
		"SELECT {POLICY_COLUMNS} FROM volume_backup_policies
		WHERE schedule IS NOT NULL AND target_id IS NOT NULL AND next_run_at <= NOW()
		ORDER BY next_run_at"
	)))
	.fetch_all(&state.db)
	.await
}

/// Advances `next_run_at` first, so a failure to queue skips one run instead of
/// retrying every minute, and a schedule that cannot be evaluated stops firing. A
/// volume that is still busy with the last run skips this one.
async fn queue_one(state: &AppState, policy: Policy) {
	let (Some(schedule), Some(target)) = (policy.schedule.as_deref(), policy.target_id) else {
		return;
	};
	let next = match next_run(schedule, OffsetDateTime::now_utc()) {
		Ok(next) => Some(next),
		Err(e) => {
			tracing::warn!(
				policy = policy.id.0,
				"disabling volume backup schedule that cannot be evaluated: {e}"
			);
			None
		}
	};
	let advanced = sqlx::query(
		"UPDATE volume_backup_policies
		SET next_run_at = $2, schedule = CASE WHEN $2::timestamptz IS NULL THEN NULL ELSE schedule END
		WHERE id = $1",
	)
	.bind(policy.id)
	.bind(next)
	.execute(&state.db)
	.await;
	if let Err(e) = advanced {
		tracing::warn!(
			policy = policy.id.0,
			"cannot advance volume backup schedule: {e}"
		);
		return;
	}
	if next.is_none() {
		return;
	}

	match busy(&state.db, policy.id).await {
		Ok(false) => {}
		Ok(true) => {
			tracing::info!(
				policy = policy.id.0,
				"skipping a scheduled volume backup: one is running"
			);
			return;
		}
		Err(e) => {
			tracing::warn!(
				policy = policy.id.0,
				"cannot check for a running volume backup: {e}"
			);
			return;
		}
	}
	// Re-read: the settings may have changed since the sweep listed it.
	let policy = match policy_by_id(&state.db, policy.id).await {
		Ok(Some(policy)) => policy,
		Ok(None) => return,
		Err(e) => {
			tracing::warn!(
				policy = policy.id.0,
				"cannot reload a volume backup policy: {e}"
			);
			return;
		}
	};
	let subject = match subject::of_policy(state, &policy).await {
		Ok(Some(subject)) => subject,
		Ok(None) => return,
		Err(e) => {
			tracing::warn!(
				policy = policy.id.0,
				"cannot resolve a volume to back up: {e}"
			);
			return;
		}
	};
	if let Err(e) = queue_backup(state, &subject, &policy, target, None).await {
		tracing::warn!(
			policy = policy.id.0,
			"cannot queue scheduled volume backup: {e}"
		);
	}
}
