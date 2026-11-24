//! Scheduled restore drills: a five-field UTC cron per database in `databases.drill_schedule`
//! (null, the default, means none) and the sweep that queues the due ones. The sweep runs
//! from the backup scheduler's minute loop.

use time::OffsetDateTime;

use crate::backups::next_run;
use crate::databases;
use crate::error::ServerError;
use crate::ids::DatabaseId;
use crate::state::AppState;

/// A schedule as typed: trimmed, and blank meaning none.
#[must_use]
pub fn normalize(schedule: Option<&str>) -> Option<String> {
	schedule
		.map(str::trim)
		.filter(|s| !s.is_empty())
		.map(str::to_owned)
}

/// When a drill on `schedule` is next due after `now`; `None` without a schedule.
///
/// # Errors
/// [`ServerError::Validation`] with the parser's message for a bad cron.
pub fn next_due(
	schedule: Option<&str>,
	now: OffsetDateTime,
) -> Result<Option<OffsetDateTime>, ServerError> {
	schedule
		.map(|s| {
			next_run(s, now)
				.map_err(|e| ServerError::Validation(format!("invalid drill schedule: {e}")))
		})
		.transpose()
}

/// A database's drill schedule and when it next fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
	pub schedule: Option<String>,
	pub next_run_at: Option<OffsetDateTime>,
}

pub async fn settings(db: &sqlx::PgPool, database: DatabaseId) -> Result<Settings, sqlx::Error> {
	sqlx::query_as!(
		Settings,
		"SELECT drill_schedule AS schedule, drill_next_run_at AS next_run_at
		FROM databases WHERE id = $1",
		database as DatabaseId,
	)
	.fetch_one(db)
	.await
}

pub async fn store(
	db: &sqlx::PgPool,
	database: DatabaseId,
	settings: &Settings,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE databases SET drill_schedule = $2, drill_next_run_at = $3 WHERE id = $1",
		database as DatabaseId,
		settings.schedule,
		settings.next_run_at,
	)
	.execute(db)
	.await?;
	Ok(())
}

struct Due {
	id: DatabaseId,
	schedule: String,
}

/// Queues every due drill. Called once a minute.
pub async fn sweep(state: &AppState) {
	let due = match sqlx::query_as!(
		Due,
		r#"
			SELECT id AS "id!: DatabaseId", drill_schedule AS "schedule!"
			FROM databases
			WHERE drill_schedule IS NOT NULL AND drill_next_run_at <= NOW()
				AND status <> 'deleting'
			ORDER BY drill_next_run_at
		"#,
	)
	.fetch_all(&state.db)
	.await
	{
		Ok(due) => due,
		Err(e) => {
			tracing::warn!("drill scheduler could not list due databases: {e}");
			return;
		}
	};
	for database in due {
		queue_one(state, &database).await;
	}
}

/// Advances `drill_next_run_at` first, so a failure to queue skips one run instead of
/// retrying every minute, and an unparsable schedule stops firing.
async fn queue_one(state: &AppState, due: &Due) {
	let next = match next_run(&due.schedule, OffsetDateTime::now_utc()) {
		Ok(next) => Some(next),
		Err(e) => {
			tracing::warn!(
				database = due.id.0,
				"disabling drill schedule that cannot be evaluated: {e}"
			);
			None
		}
	};
	if let Err(e) = sqlx::query!(
		"UPDATE databases SET drill_next_run_at = $2 WHERE id = $1",
		due.id as DatabaseId,
		next,
	)
	.execute(&state.db)
	.await
	{
		tracing::warn!(database = due.id.0, "cannot advance drill schedule: {e}");
		return;
	}
	if next.is_none() {
		return;
	}

	let database = match databases::find_by_id(&state.db, due.id).await {
		Ok(Some(database)) => database,
		Ok(None) => return,
		Err(e) => {
			tracing::warn!(database = due.id.0, "cannot load database for a drill: {e}");
			return;
		}
	};
	match super::queue(state, &database, None).await {
		// A conflict means the previous drill is still going: this run is skipped, not stacked.
		Ok(_) | Err(ServerError::Conflict(_)) => {}
		Err(ServerError::Validation(m)) if m == super::NO_BACKUP => {
			super::record_without_backup(state, &database).await;
		}
		Err(e) => tracing::warn!(database = due.id.0, "cannot queue scheduled drill: {e}"),
	}
}
