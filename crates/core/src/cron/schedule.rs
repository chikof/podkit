//! Schedules of scheduled tasks: five-field cron read in an IANA time zone.
//!
//! The time zone database is the one `jiff` bundles, so a minimal container needs no
//! `/usr/share/zoneinfo`. Daylight saving follows croner's rules: a wall-clock time that
//! does not exist on the spring-forward day runs once at the first instant after the gap,
//! and one that happens twice on the fall-back day runs once.

use croner::Cron;
use croner::parser::{CronParser, Seconds};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use time::OffsetDateTime;

/// How many upcoming runs the forms show.
pub const PREVIEW_RUNS: usize = 5;

/// The longest zone name accepted; the longest IANA name is 30 characters.
const MAX_ZONE: usize = 64;

fn parse(schedule: &str) -> Result<Cron, String> {
	CronParser::builder()
		.seconds(Seconds::Disallowed)
		.build()
		.parse(schedule.trim())
		.map_err(|e| e.to_string())
}

/// The zone called `name` (`UTC`, `Europe/Berlin`).
///
/// # Errors
/// A message when no such zone exists.
pub fn zone(name: &str) -> Result<TimeZone, String> {
	if name.is_empty() || name.len() > MAX_ZONE {
		return Err("the time zone must be an IANA name such as Europe/Berlin".to_owned());
	}
	TimeZone::get(name).map_err(|_| format!("{name} is not a known time zone"))
}

/// Checks that `schedule` is five cron fields and `timezone` a known zone.
///
/// # Errors
/// The first problem found, in words for the person who typed it.
pub fn validate(schedule: &str, timezone: &str) -> Result<(), String> {
	parse(schedule)?;
	zone(timezone).map(drop)
}

/// The first `count` runs of `schedule` after `after`, in the zone `timezone`.
///
/// # Errors
/// A message when the expression or the zone is invalid, or the schedule never fires.
pub fn upcoming(
	schedule: &str,
	timezone: &str,
	after: OffsetDateTime,
	count: usize,
) -> Result<Vec<OffsetDateTime>, String> {
	let cron = parse(schedule)?;
	let zone = zone(timezone)?;
	let mut at = Timestamp::from_second(after.unix_timestamp())
		.map_err(|e| e.to_string())?
		.to_zoned(zone);
	let mut runs = Vec::with_capacity(count);
	for _ in 0..count {
		at = cron
			.find_next_occurrence(&at, false)
			.map_err(|e| e.to_string())?;
		runs.push(
			OffsetDateTime::from_unix_timestamp(at.timestamp().as_second())
				.map_err(|e| e.to_string())?,
		);
	}
	Ok(runs)
}

/// The first run strictly after `after`.
///
/// # Errors
/// As [`upcoming`].
pub fn next_after(
	schedule: &str,
	timezone: &str,
	after: OffsetDateTime,
) -> Result<OffsetDateTime, String> {
	upcoming(schedule, timezone, after, 1)?
		.pop()
		.ok_or_else(|| "the schedule never fires".to_owned())
}

/// `2026-11-21 03:00 CET`: an instant as the wall clock of `timezone` shows it, for the
/// pages. A zone that is not known falls back to UTC.
#[must_use]
pub fn local_text(at: OffsetDateTime, timezone: &str) -> String {
	let zone = zone(timezone).unwrap_or(TimeZone::UTC);
	match Timestamp::from_second(at.unix_timestamp()) {
		Ok(ts) => ts.to_zoned(zone).strftime("%Y-%m-%d %H:%M %Z").to_string(),
		Err(_) => String::new(),
	}
}

#[cfg(test)]
mod tests {
	use time::macros::datetime;

	use super::*;

	#[test]
	fn a_schedule_is_read_in_its_time_zone() {
		let after = datetime!(2026-11-20 12:00 UTC);
		// 03:00 in Berlin is 02:00 UTC in November (CET, UTC+1).
		assert_eq!(
			next_after("0 3 * * *", "Europe/Berlin", after).unwrap(),
			datetime!(2026-11-21 02:00 UTC)
		);
		assert_eq!(
			next_after("0 3 * * *", "UTC", after).unwrap(),
			datetime!(2026-11-21 03:00 UTC)
		);
		// New York is UTC-5 in November.
		assert_eq!(
			next_after("0 3 * * *", "America/New_York", after).unwrap(),
			datetime!(2026-11-21 08:00 UTC)
		);
		// Kolkata has no daylight saving and sits at UTC+5:30.
		assert_eq!(
			next_after("0 3 * * *", "Asia/Kolkata", after).unwrap(),
			datetime!(2026-11-20 21:30 UTC)
		);
	}

	#[test]
	fn the_offset_follows_daylight_saving() {
		// Berlin springs forward on 2027-03-28: 03:00 is UTC+1 before and UTC+2 after.
		let runs = upcoming(
			"0 3 * * *",
			"Europe/Berlin",
			datetime!(2027-03-26 12:00 UTC),
			3,
		)
		.unwrap();
		assert_eq!(
			runs,
			[
				datetime!(2027-03-27 02:00 UTC),
				// 02:00 to 03:00 does not exist on the 28th; 03:00 is the first valid time.
				datetime!(2027-03-28 01:00 UTC),
				datetime!(2027-03-29 01:00 UTC),
			]
		);
	}

	#[test]
	fn a_time_inside_the_spring_gap_runs_once() {
		// 02:30 does not exist in Berlin on 2027-03-28.
		let runs = upcoming(
			"30 2 * * *",
			"Europe/Berlin",
			datetime!(2027-03-27 12:00 UTC),
			2,
		)
		.unwrap();
		assert_eq!(runs.len(), 2);
		assert_eq!(runs[0].date(), time::macros::date!(2027 - 03 - 28));
		assert_eq!(runs[1].date(), time::macros::date!(2027 - 03 - 29));
	}

	#[test]
	fn a_time_in_the_autumn_overlap_runs_once() {
		// 02:30 happens twice in Berlin on 2026-10-25.
		let runs = upcoming(
			"30 2 * * *",
			"Europe/Berlin",
			datetime!(2026-10-24 12:00 UTC),
			3,
		)
		.unwrap();
		let days: Vec<_> = runs.iter().map(|r| r.date().day()).collect();
		assert_eq!(days, [25, 26, 27], "{runs:?}");
	}

	#[test]
	fn the_next_five_runs_are_listed_in_order() {
		let runs = upcoming(
			"*/15 * * * *",
			"UTC",
			datetime!(2026-11-20 12:07:30 UTC),
			PREVIEW_RUNS,
		)
		.unwrap();
		assert_eq!(
			runs,
			[
				datetime!(2026-11-20 12:15 UTC),
				datetime!(2026-11-20 12:30 UTC),
				datetime!(2026-11-20 12:45 UTC),
				datetime!(2026-11-20 13:00 UTC),
				datetime!(2026-11-20 13:15 UTC),
			]
		);
	}

	#[test]
	fn bad_input_is_refused_with_a_reason() {
		assert!(validate("0 3 * * *", "Europe/Berlin").is_ok());
		assert!(validate("0 0 3 * * *", "UTC").is_err(), "a seconds field");
		assert!(validate("every day", "UTC").is_err());
		assert!(validate("", "UTC").is_err());
		let why = validate("0 3 * * *", "Mars/Olympus").unwrap_err();
		assert!(why.contains("Mars/Olympus"), "{why}");
		assert!(validate("0 3 * * *", "").is_err());
		assert!(validate("0 3 * * *", &"x".repeat(100)).is_err());
	}
}
