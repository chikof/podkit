//! The scheduler: queues a run for every task whose time has come.
//!
//! It is not a service of its own. The job dispatcher's loop calls [`tick`] every few
//! seconds, and the hourly maintenance calls [`purge`]. Two podkit processes over one
//! database are safe: a sweep runs in one transaction that takes a transaction-scoped
//! advisory lock first, so only one of them is the leader for that moment and the other
//! finds the lock taken and does nothing. The lock goes with the transaction, so a
//! leader that dies hands over by itself.
//!
//! A task that was due while podkit was down runs once when it is back, however many slots
//! it missed (a "catch up once" policy), then follows its schedule from now on.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use time::OffsetDateTime;

use super::{Concurrency, CronTask, Trigger, schedule, store};
use crate::state::AppState;

/// Advisory lock key of the leader: "podkit" and a counter.
const LEADER_LOCK: i64 = 0x706f_646b_6974_0001;
/// Least time between two sweeps.
const TICK_SECONDS: i64 = 10;
/// A run this much later than its slot is a catch-up, not a scheduled run.
const LATE: Duration = Duration::from_secs(120);
/// Tasks handled by one sweep; the rest wait for the next one.
const BATCH: i64 = 100;
/// Runs a `queue` task may have waiting; more are skipped, or a slow task would pile up.
pub const MAX_QUEUED: i64 = 5;
/// Runs kept for each task, and for how long.
const KEEP_RUNS: i64 = 200;
const KEEP_DAYS: i32 = 30;

static LAST_TICK: AtomicI64 = AtomicI64::new(0);

/// Sweeps if the last sweep was long enough ago. Cheap when nothing is due: one indexed
/// query.
pub async fn tick(state: &AppState) {
	let now = OffsetDateTime::now_utc();
	let last = LAST_TICK.load(Ordering::Relaxed);
	if now.unix_timestamp() - last < TICK_SECONDS
		|| LAST_TICK
			.compare_exchange(
				last,
				now.unix_timestamp(),
				Ordering::Relaxed,
				Ordering::Relaxed,
			)
			.is_err()
	{
		return;
	}
	if let Err(e) = sweep(state, now).await {
		tracing::warn!("cron sweep failed: {e}");
	}
}

/// Queues the runs due at `now` and moves each task on to its next slot. Returns how many
/// runs it recorded, skipped ones included; zero when another process holds the lock.
///
/// # Errors
/// A database error; nothing is changed then.
pub async fn sweep(state: &AppState, now: OffsetDateTime) -> Result<usize, sqlx::Error> {
	let mut tx = state.db.begin().await?;
	let leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
		.bind(LEADER_LOCK)
		.fetch_one(&mut *tx)
		.await?;
	if !leader {
		return Ok(0);
	}
	let due: Vec<CronTask> = sqlx::query_as(concat!(
		"SELECT ",
		super::task_cols!(),
		" FROM cron_tasks WHERE enabled AND next_run_at <= $1
		ORDER BY next_run_at LIMIT $2 FOR UPDATE SKIP LOCKED"
	))
	.bind(now)
	.bind(BATCH)
	.fetch_all(&mut *tx)
	.await?;

	let mut recorded = 0;
	let mut queued = false;
	for task in &due {
		let slot = task.next_run_at;
		let late = slot.is_some_and(|s| now - s > LATE);
		let trigger = if late {
			Trigger::CatchUp
		} else {
			Trigger::Schedule
		};
		// Moved on first: whatever happens next, this slot is not tried again.
		let next = match schedule::next_after(&task.schedule, &task.timezone, now) {
			Ok(next) => Some(next),
			Err(e) => {
				tracing::warn!(task = task.id.0, "a cron schedule cannot be evaluated: {e}");
				None
			}
		};
		sqlx::query("UPDATE cron_tasks SET next_run_at = $2 WHERE id = $1")
			.bind(task.id)
			.bind(next)
			.execute(&mut *tx)
			.await?;

		let (active, waiting) = store::active_counts(&mut *tx, task.id).await?;
		let why = match task.concurrency {
			Concurrency::Skip if active > 0 => Some("the previous run was still going"),
			Concurrency::Queue if waiting >= MAX_QUEUED => {
				Some("too many runs were already waiting")
			}
			_ => None,
		};
		if let Some(why) = why {
			store::insert_skipped(&mut tx, task, trigger, slot, why).await?;
		} else {
			store::enqueue_run(&mut tx, task, trigger, 1, slot, now).await?;
			queued = true;
		}
		recorded += 1;
	}
	tx.commit().await?;
	if queued {
		state.jobs.notify.notify_one();
	}
	Ok(recorded)
}

/// Deletes the runs a task no longer needs to show: finished more than 30 days ago, or
/// beyond its newest 200. Their jobs and log files go too.
pub async fn purge(state: &AppState) {
	if let Err(e) = purge_inner(state).await {
		tracing::warn!("failed to purge old cron runs: {e}");
	}
}

async fn purge_inner(state: &AppState) -> Result<(), sqlx::Error> {
	let rows: Vec<(i64, Option<i64>)> = sqlx::query_as(
		"WITH ranked AS (
			SELECT id, job_id, finished_at,
				ROW_NUMBER() OVER (PARTITION BY task_id ORDER BY id DESC) AS rank
			FROM cron_runs WHERE status NOT IN ('queued', 'running')
		)
		DELETE FROM cron_runs WHERE id IN (
			SELECT id FROM ranked
			WHERE rank > $1 OR finished_at < NOW() - make_interval(days => $2)
		) RETURNING id, job_id",
	)
	.bind(KEEP_RUNS)
	.bind(KEEP_DAYS)
	.fetch_all(&state.db)
	.await?;
	let jobs: Vec<i64> = rows.iter().filter_map(|(_, job)| *job).collect();
	if jobs.is_empty() {
		return Ok(());
	}
	sqlx::query("DELETE FROM jobs WHERE id = ANY($1) AND status NOT IN ('queued', 'running')")
		.bind(&jobs)
		.execute(&state.db)
		.await?;
	for job in jobs {
		let path = state.logs.path(crate::ids::JobId(job));
		if let Err(e) = tokio::fs::remove_file(&path).await
			&& e.kind() != std::io::ErrorKind::NotFound
		{
			tracing::warn!("cannot remove cron log {}: {e}", path.display());
		}
	}
	Ok(())
}
