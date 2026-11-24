//! Scheduled backups: the 5-field UTC cron helpers and the loop that queues
//! a backup for every database whose schedule is due, and then the due restore
//! drills ([`super::drills::schedule`]).

use std::cmp::Ordering;
use std::time::Duration;

use croner::errors::CronError;
use croner::parser::{CronParser, Seconds};
use croner::{CivilDate, CivilDateTime, CivilTime, Cron, CronDateTime, Resolution, Weekday};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};
use tokio_util::sync::CancellationToken;

use crate::ids::{BackupTargetId, DatabaseId, ServerId};
use crate::state::AppState;

const INTERVAL: Duration = Duration::from_mins(1);

/// UTC wall clock for croner, over the `time` crate the rest of the server
/// uses. UTC has no gaps or folds, so every civil time resolves exactly once.
#[derive(Clone, Copy)]
struct Utc(OffsetDateTime);

impl CronDateTime for Utc {
	fn to_civil(&self) -> CivilDateTime {
		let t = self.0;
		CivilDateTime::new(
			CivilDate::from_parts_unchecked(
				t.year(),
				u32::from(u8::from(t.month())),
				u32::from(t.day()),
			),
			CivilTime::from_parts_unchecked(
				u32::from(t.hour()),
				u32::from(t.minute()),
				u32::from(t.second()),
			),
		)
	}

	fn civil_weekday(&self) -> Weekday {
		Weekday::from_days_from_sunday(u32::from(self.0.weekday().number_days_from_sunday()))
	}

	fn resolve_civil(&self, civil: CivilDateTime) -> Result<Resolution<Self>, CronError> {
		let part = |n: u32, err: CronError| u8::try_from(n).map_err(|_| err);
		let month = Month::try_from(part(civil.month(), CronError::InvalidDate)?)
			.map_err(|_| CronError::InvalidDate)?;
		let date = Date::from_calendar_date(
			civil.year(),
			month,
			part(civil.day(), CronError::InvalidDate)?,
		)
		.map_err(|_| CronError::InvalidDate)?;
		let time = Time::from_hms(
			part(civil.hour(), CronError::InvalidTime)?,
			part(civil.minute(), CronError::InvalidTime)?,
			part(civil.second(), CronError::InvalidTime)?,
		)
		.map_err(|_| CronError::InvalidTime)?;
		Ok(Resolution::Single(Self(
			PrimitiveDateTime::new(date, time).assume_utc(),
		)))
	}

	fn checked_add_seconds(&self, seconds: i64) -> Option<Self> {
		self.0
			.checked_add(time::Duration::seconds(seconds))
			.map(Self)
	}

	fn cmp_instant(&self, other: &Self) -> Ordering {
		self.0.cmp(&other.0)
	}
}

/// Exactly five fields: minute, hour, day of month, month, day of week. A
/// seconds or year field is refused.
fn parse(schedule: &str) -> Result<Cron, String> {
	CronParser::builder()
		.seconds(Seconds::Disallowed)
		.build()
		.parse(schedule)
		.map_err(|e| e.to_string())
}

/// Checks that `schedule` is a valid 5-field cron expression.
///
/// # Errors
/// The parser's message.
pub fn parse_schedule(schedule: &str) -> Result<(), String> {
	parse(schedule).map(drop)
}

/// The first occurrence of `schedule`, in UTC, strictly after `after`.
///
/// # Errors
/// The parser's message when `schedule` is invalid or never fires.
pub fn next_run(schedule: &str, after: OffsetDateTime) -> Result<OffsetDateTime, String> {
	let cron = parse(schedule)?;
	// Whole seconds in UTC: cron never matches below a second.
	let after =
		OffsetDateTime::from_unix_timestamp(after.unix_timestamp()).map_err(|e| e.to_string())?;
	cron.find_next_occurrence(&Utc(after), false)
		.map(|next| next.0)
		.map_err(|e| e.to_string())
}

struct Due {
	id: DatabaseId,
	server_id: ServerId,
	target_id: BackupTargetId,
	schedule: String,
}

/// Queues the due backups and restore drills once a minute until `shutdown`.
pub async fn run(state: AppState, shutdown: CancellationToken) {
	loop {
		sweep(&state).await;
		super::drills::schedule::sweep(&state).await;
		tokio::select! {
			() = shutdown.cancelled() => return,
			() = tokio::time::sleep(INTERVAL) => {}
		}
	}
}

async fn sweep(state: &AppState) {
	let due = match due(state).await {
		Ok(due) => due,
		Err(e) => {
			tracing::warn!("backup scheduler could not list due databases: {e}");
			return;
		}
	};
	for database in due {
		queue_one(state, &database).await;
	}
}

async fn due(state: &AppState) -> Result<Vec<Due>, sqlx::Error> {
	sqlx::query_as!(
		Due,
		r#"
			SELECT id AS "id!: DatabaseId", server_id AS "server_id!: ServerId",
				backup_target_id AS "target_id!: BackupTargetId", backup_schedule AS "schedule!"
			FROM databases
			WHERE backup_schedule IS NOT NULL AND backup_target_id IS NOT NULL
				AND status = 'running' AND backup_next_run_at <= NOW()
			ORDER BY backup_next_run_at
		"#,
	)
	.fetch_all(&state.db)
	.await
}

/// Advances `backup_next_run_at` first, so a failure to queue skips one run
/// instead of retrying every minute, and an unparsable schedule stops firing.
async fn queue_one(state: &AppState, database: &Due) {
	let next = match next_run(&database.schedule, OffsetDateTime::now_utc()) {
		Ok(next) => Some(next),
		Err(e) => {
			tracing::warn!(
				database = database.id.0,
				"disabling backup schedule that cannot be evaluated: {e}"
			);
			None
		}
	};
	if let Err(e) = sqlx::query!(
		"UPDATE databases SET backup_next_run_at = $2 WHERE id = $1",
		database.id as DatabaseId,
		next,
	)
	.execute(&state.db)
	.await
	{
		tracing::warn!(
			database = database.id.0,
			"cannot advance backup schedule: {e}"
		);
		return;
	}
	if next.is_none() {
		return;
	}

	if let Err(e) = super::queue(
		state,
		database.id,
		database.server_id,
		database.target_id,
		None,
	)
	.await
	{
		tracing::warn!(
			database = database.id.0,
			"cannot queue scheduled backup: {e}"
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn at(year: i32, month: Month, day: u8, hour: u8, minute: u8, second: u8) -> OffsetDateTime {
		PrimitiveDateTime::new(
			Date::from_calendar_date(year, month, day).unwrap(),
			Time::from_hms(hour, minute, second).unwrap(),
		)
		.assume_utc()
	}

	#[test]
	fn every_minute_fires_on_the_next_minute() {
		let next = next_run("* * * * *", at(2026, Month::November, 17, 12, 0, 30)).unwrap();
		assert_eq!(next, at(2026, Month::November, 17, 12, 1, 0));
	}

	#[test]
	fn every_minute_is_strictly_after_an_exact_minute() {
		let next = next_run("* * * * *", at(2026, Month::November, 17, 12, 0, 0)).unwrap();
		assert_eq!(next, at(2026, Month::November, 17, 12, 1, 0));
	}

	#[test]
	fn daily_at_three_fires_the_same_day_when_still_ahead() {
		let next = next_run("0 3 * * *", at(2026, Month::November, 17, 2, 59, 59)).unwrap();
		assert_eq!(next, at(2026, Month::November, 17, 3, 0, 0));
	}

	#[test]
	fn daily_at_three_rolls_to_the_next_day_and_month() {
		let next = next_run("0 3 * * *", at(2026, Month::November, 17, 12, 0, 0)).unwrap();
		assert_eq!(next, at(2026, Month::November, 18, 3, 0, 0));
		let next = next_run("0 3 * * *", at(2026, Month::November, 30, 3, 0, 0)).unwrap();
		assert_eq!(next, at(2026, Month::December, 1, 3, 0, 0));
	}

	#[test]
	fn day_of_week_field_is_honoured() {
		// 2026-11-17 is a Tuesday; the next Friday is the 20th.
		let next = next_run("30 4 * * FRI", at(2026, Month::November, 17, 12, 0, 0)).unwrap();
		assert_eq!(next, at(2026, Month::November, 20, 4, 30, 0));
	}

	#[test]
	fn a_non_utc_start_is_read_as_the_same_instant() {
		let utc = at(2026, Month::November, 17, 2, 30, 0);
		let shifted = utc.to_offset(time::UtcOffset::from_hms(5, 0, 0).unwrap());
		assert_eq!(
			next_run("0 3 * * *", shifted).unwrap(),
			at(2026, Month::November, 17, 3, 0, 0)
		);
	}

	#[test]
	fn only_five_field_expressions_are_accepted() {
		assert!(parse_schedule("0 3 * * *").is_ok());
		assert!(parse_schedule("*/15 * * * *").is_ok());
		assert!(parse_schedule("0 0 3 * * *").is_err());
		assert!(parse_schedule("0 3 * *").is_err());
		assert!(parse_schedule("").is_err());
		assert!(parse_schedule("every day").is_err());
		assert!(parse_schedule("61 * * * *").is_err());
	}
}
