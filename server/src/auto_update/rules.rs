//! The decisions of image auto-update that need no database and no network: when to look
//! next, whether the digest a tag names should deploy, and what became of the last
//! auto-update. Everything here is a function of its arguments, so the tests give it the
//! clock.

use time::{Duration, OffsetDateTime};

/// How often a tag is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
	Hourly,
	SixHours,
	Daily,
}

impl Interval {
	pub const ALL: [Self; 3] = [Self::Hourly, Self::SixHours, Self::Daily];

	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Hourly => "hourly",
			Self::SixHours => "6h",
			Self::Daily => "daily",
		}
	}

	#[must_use]
	pub fn parse(raw: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|i| i.as_str() == raw.trim())
	}

	/// What a page calls it.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Hourly => "Every hour",
			Self::SixHours => "Every 6 hours",
			Self::Daily => "Every day",
		}
	}

	#[must_use]
	pub const fn every(self) -> Duration {
		match self {
			Self::Hourly => Duration::hours(1),
			Self::SixHours => Duration::hours(6),
			Self::Daily => Duration::days(1),
		}
	}
}

const DAY: i64 = 24 * 60;

/// A daily maintenance window: minutes of the day in a fixed offset from UTC. The end may be
/// before the start, which wraps over midnight (22:00 to 04:00).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
	pub start: i64,
	pub end: i64,
	/// Minutes ahead of UTC.
	pub offset: i64,
}

impl Window {
	fn minute_of_day(&self, at: OffsetDateTime) -> i64 {
		(at.unix_timestamp().div_euclid(60) + self.offset).rem_euclid(DAY)
	}

	#[must_use]
	pub fn open_at(&self, at: OffsetDateTime) -> bool {
		let m = self.minute_of_day(at);
		if self.start < self.end {
			self.start <= m && m < self.end
		} else {
			m >= self.start || m < self.end
		}
	}

	/// The first moment at or after `at` when the window is open.
	#[must_use]
	pub fn next_open(&self, at: OffsetDateTime) -> OffsetDateTime {
		if self.open_at(at) {
			return at;
		}
		let wait = (self.start - self.minute_of_day(at)).rem_euclid(DAY);
		let minute = at.unix_timestamp().div_euclid(60) * 60;
		OffsetDateTime::from_unix_timestamp(minute + wait * 60).unwrap_or(at)
	}
}

/// `16:00` as a minute of the day.
#[must_use]
pub fn parse_time(raw: &str) -> Option<i64> {
	let (h, m) = raw.trim().split_once(':')?;
	let (h, m) = (h.parse::<i64>().ok()?, m.get(..2)?.parse::<i64>().ok()?);
	((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

/// `04:30` for 270.
#[must_use]
pub fn time_label(minute: i64) -> String {
	let m = minute.rem_euclid(DAY);
	format!("{:02}:{:02}", m / 60, m % 60)
}

/// After a check that could not do its job (a deploy in progress, a freeze), the next look.
pub const RETRY: Duration = Duration::minutes(15);
/// After a check that could not reach the registry, the next look at most this far away.
pub const ERROR_RETRY: Duration = Duration::hours(1);

/// When to look next: `after` from `now`, moved to the next opening of the window if there is
/// one and that moment is outside it.
#[must_use]
pub fn next_check(now: OffsetDateTime, after: Duration, window: Option<Window>) -> OffsetDateTime {
	let at = now + after;
	window.map_or(at, |w| w.next_open(at))
}

/// `sha256:` and 64 lowercase hex digits (or `sha512:` and 128).
#[must_use]
pub fn valid_digest(digest: &str) -> bool {
	let hex = |s: &str, n: usize| {
		s.len() == n
			&& s.bytes()
				.all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
	};
	match digest.split_once(':') {
		Some(("sha256", h)) => hex(h, 64),
		Some(("sha512", h)) => hex(h, 128),
		_ => false,
	}
}

/// The digest of a `repo@sha256:…` reference, the form a deploy of an image records.
#[must_use]
pub fn digest_of(reference: &str) -> Option<&str> {
	reference
		.rsplit_once('@')
		.map(|(_, d)| d)
		.filter(|d| valid_digest(d))
}

/// Every digest the running image is known by: the one its deployment recorded and the ones
/// the server reports (an index and its platform manifest are both the same image), each once.
#[must_use]
pub fn running_digests(recorded: Option<&str>, reported: &[String]) -> Vec<String> {
	let mut out: Vec<String> = Vec::new();
	for d in recorded
		.and_then(digest_of)
		.into_iter()
		.chain(reported.iter().map(String::as_str))
	{
		if valid_digest(d) && !out.iter().any(|x| x == d) {
			out.push(d.to_owned());
		}
	}
	out
}

/// Whether an image reference has a tag to follow.
///
/// # Errors
/// What to tell the user.
pub fn trackable(image: &str) -> Result<(), &'static str> {
	if image.contains('@') {
		Err("The image is pinned by digest, so it has no tag to follow. Use a tag such as :latest.")
	} else if image.trim().is_empty() {
		Err("The application has no image.")
	} else {
		Ok(())
	}
}

/// What became of a deployment auto-update queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
	/// Still queued, building or deploying.
	InProgress,
	/// It went live, and nothing has taken it back.
	Good,
	/// It failed, was cancelled, crashed, never got healthy or was rolled back.
	Bad,
}

/// The outcome from the deployment's status and its health gate state (`deployment_health`).
#[must_use]
pub fn outcome(status: &str, health: Option<&str>) -> Outcome {
	if matches!(health, Some("rolled_back" | "unhealthy")) {
		return Outcome::Bad;
	}
	match status {
		"queued" | "building" | "deploying" => Outcome::InProgress,
		"failed" | "cancelled" | "crashed" => Outcome::Bad,
		_ => Outcome::Good,
	}
}

/// The digest to hold back after this check: the one of the last auto-update when it went bad
/// (else the one already held back), and only while the tag still names it. Once the tag moves
/// on it is forgotten.
#[must_use]
pub fn bad_digest(
	stored: Option<&str>,
	deployed: Option<(&str, Outcome)>,
	remote: &str,
) -> Option<String> {
	let bad = match deployed {
		Some((digest, Outcome::Bad)) => Some(digest),
		_ => stored,
	};
	bad.filter(|b| *b == remote).map(str::to_owned)
}

/// What one check knows.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools, reason = "one fact per field")]
pub struct Facts<'a> {
	/// The digest the tag names now.
	pub remote: &'a str,
	/// The digests the running image is known by.
	pub running: &'a [String],
	/// The application has a running deployment.
	pub app_running: bool,
	/// The digest of the last auto-update and what became of it.
	pub deployed: Option<(&'a str, Outcome)>,
	/// The digest held back, from [`bad_digest`].
	pub bad: Option<&'a str>,
	/// A deployment of the application is queued or in progress.
	pub busy: bool,
	/// An autodeploy freeze window is open.
	pub frozen: bool,
	/// Inside the maintenance window, or none is set, or someone pressed Check now.
	pub in_window: bool,
}

/// What a check does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
	UpToDate,
	Deploy,
	SkipBad,
	Busy,
	Frozen,
	OutsideWindow,
	NotRunning,
}

/// Whether the digest the tag names should deploy. In order: nothing runs, so there is nothing
/// to update; the running image is that digest; auto-update already deployed it (so a
/// maintainer's rollback holds until the tag moves); it went bad before; something else is
/// deploying; a freeze is open; outside the window. Only then it deploys.
#[must_use]
pub fn decide(f: &Facts<'_>) -> Decision {
	if !f.app_running {
		return Decision::NotRunning;
	}
	if f.running.iter().any(|d| d == f.remote) {
		return Decision::UpToDate;
	}
	match f.deployed {
		Some((d, Outcome::Good)) if d == f.remote => return Decision::UpToDate,
		Some((d, Outcome::InProgress)) if d == f.remote => return Decision::Busy,
		_ => {}
	}
	if f.bad == Some(f.remote) {
		Decision::SkipBad
	} else if f.busy {
		Decision::Busy
	} else if f.frozen {
		Decision::Frozen
	} else if !f.in_window {
		Decision::OutsideWindow
	} else {
		Decision::Deploy
	}
}

/// What the last check came to, as stored in `last_result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
	UpToDate,
	Deployed,
	SkippedBad,
	Busy,
	Frozen,
	OutsideWindow,
	NotRunning,
	Error,
}

impl Checked {
	const ALL: [Self; 8] = [
		Self::UpToDate,
		Self::Deployed,
		Self::SkippedBad,
		Self::Busy,
		Self::Frozen,
		Self::OutsideWindow,
		Self::NotRunning,
		Self::Error,
	];

	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::UpToDate => "up_to_date",
			Self::Deployed => "deployed",
			Self::SkippedBad => "skipped_bad",
			Self::Busy => "busy",
			Self::Frozen => "frozen",
			Self::OutsideWindow => "outside_window",
			Self::NotRunning => "not_running",
			Self::Error => "error",
		}
	}

	#[must_use]
	pub fn from_stored(raw: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|c| c.as_str() == raw)
	}

	/// What a page calls it.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::UpToDate => "Up to date",
			Self::Deployed => "New image deployed",
			Self::SkippedBad => "Held back",
			Self::Busy => "Waiting for a deploy",
			Self::Frozen => "Held by a freeze",
			Self::OutsideWindow => "Outside the window",
			Self::NotRunning => "Not running",
			Self::Error => "Check failed",
		}
	}

	/// How long until the next look: the interval when the check settled something, sooner
	/// when it only waited.
	#[must_use]
	pub fn delay(self, interval: Interval) -> Duration {
		match self {
			Self::UpToDate | Self::Deployed | Self::SkippedBad | Self::NotRunning => {
				interval.every()
			}
			Self::Busy | Self::Frozen | Self::OutsideWindow => RETRY,
			Self::Error => interval.every().min(ERROR_RETRY),
		}
	}
}

impl From<Decision> for Checked {
	fn from(d: Decision) -> Self {
		match d {
			Decision::UpToDate => Self::UpToDate,
			Decision::Deploy => Self::Deployed,
			Decision::SkipBad => Self::SkippedBad,
			Decision::Busy => Self::Busy,
			Decision::Frozen => Self::Frozen,
			Decision::OutsideWindow => Self::OutsideWindow,
			Decision::NotRunning => Self::NotRunning,
		}
	}
}

/// `sha256:1a2b3c4d5e6f` for a page.
#[must_use]
pub fn short_digest(digest: &str) -> String {
	match digest.split_once(':') {
		Some((algo, hex)) => format!("{algo}:{}", hex.chars().take(12).collect::<String>()),
		None => digest.chars().take(19).collect(),
	}
}

#[cfg(test)]
mod tests {
	use time::macros::datetime;

	use super::*;

	fn d(c: char) -> String {
		format!("sha256:{}", c.to_string().repeat(64))
	}

	fn facts<'a>(remote: &'a str, running: &'a [String]) -> Facts<'a> {
		Facts {
			remote,
			running,
			app_running: true,
			deployed: None,
			bad: None,
			busy: false,
			frozen: false,
			in_window: true,
		}
	}

	#[test]
	fn intervals_round_trip_and_measure() {
		for i in Interval::ALL {
			assert_eq!(Interval::parse(i.as_str()), Some(i));
		}
		assert_eq!(Interval::parse("weekly"), None);
		assert_eq!(Interval::SixHours.every(), Duration::hours(6));
	}

	#[test]
	fn a_tag_that_moved_deploys() {
		let (old, new) = (d('a'), d('b'));
		let running = [old];
		assert_eq!(decide(&facts(&new, &running)), Decision::Deploy);
	}

	#[test]
	fn the_running_digest_is_up_to_date_by_any_of_its_names() {
		let (index, platform) = (d('a'), d('b'));
		let running = running_digests(
			Some(&format!("ghcr.io/me/app@{platform}")),
			std::slice::from_ref(&index),
		);
		assert_eq!(running, vec![platform.clone(), index.clone()]);
		assert_eq!(decide(&facts(&index, &running)), Decision::UpToDate);
		assert_eq!(decide(&facts(&platform, &running)), Decision::UpToDate);
	}

	#[test]
	fn nothing_running_is_not_updated() {
		let running: [String; 0] = [];
		let mut f = facts("sha256:x", &running);
		f.app_running = false;
		assert_eq!(decide(&f), Decision::NotRunning);
	}

	#[test]
	fn a_bad_digest_is_skipped_until_the_tag_moves() {
		let (old, bad, next) = (d('a'), d('b'), d('c'));
		let running = [old];
		// The auto-update of `bad` was rolled back: the old image runs again.
		let deployed = Some((bad.as_str(), Outcome::Bad));
		let held = bad_digest(None, deployed, &bad);
		assert_eq!(held.as_deref(), Some(bad.as_str()));
		let mut f = facts(&bad, &running);
		f.deployed = deployed;
		f.bad = held.as_deref();
		assert_eq!(decide(&f), Decision::SkipBad);
		// Still held at the next check, from the stored value alone.
		assert_eq!(
			bad_digest(Some(&bad), None, &bad).as_deref(),
			Some(bad.as_str())
		);
		// The tag moves on: forgotten, and the new digest deploys.
		assert_eq!(bad_digest(Some(&bad), deployed, &next), None);
		let mut f = facts(&next, &running);
		f.deployed = deployed;
		assert_eq!(decide(&f), Decision::Deploy);
	}

	#[test]
	fn a_digest_auto_update_deployed_is_not_deployed_again() {
		let (old, new) = (d('a'), d('b'));
		// A maintainer rolled back by hand: the old image runs, the tag still names the new one.
		let running = [old];
		let mut f = facts(&new, &running);
		f.deployed = Some((&new, Outcome::Good));
		assert_eq!(decide(&f), Decision::UpToDate);
		f.deployed = Some((&new, Outcome::InProgress));
		assert_eq!(decide(&f), Decision::Busy);
	}

	#[test]
	fn busy_frozen_and_window_hold_in_that_order() {
		let (old, new) = (d('a'), d('b'));
		let running = [old];
		let mut f = facts(&new, &running);
		f.in_window = false;
		assert_eq!(decide(&f), Decision::OutsideWindow);
		f.frozen = true;
		assert_eq!(decide(&f), Decision::Frozen);
		f.busy = true;
		assert_eq!(decide(&f), Decision::Busy);
		f.bad = Some(&new);
		assert_eq!(decide(&f), Decision::SkipBad);
	}

	#[test]
	fn outcomes_read_status_and_health() {
		assert_eq!(outcome("running", None), Outcome::Good);
		assert_eq!(outcome("running", Some("healthy")), Outcome::Good);
		assert_eq!(outcome("stopped", None), Outcome::Good);
		assert_eq!(outcome("stopped", Some("rolled_back")), Outcome::Bad);
		assert_eq!(outcome("running", Some("unhealthy")), Outcome::Bad);
		assert_eq!(outcome("failed", None), Outcome::Bad);
		assert_eq!(outcome("cancelled", None), Outcome::Bad);
		assert_eq!(outcome("crashed", None), Outcome::Bad);
		assert_eq!(outcome("deploying", Some("waiting")), Outcome::InProgress);
	}

	#[test]
	fn a_window_opens_daily_and_wraps_midnight() {
		let night = Window {
			start: 22 * 60,
			end: 4 * 60,
			offset: 0,
		};
		assert!(night.open_at(datetime!(2026-01-05 23:00 UTC)));
		assert!(night.open_at(datetime!(2026-01-06 03:59 UTC)));
		assert!(!night.open_at(datetime!(2026-01-06 04:00 UTC)));
		assert_eq!(
			night.next_open(datetime!(2026-01-06 09:30:15 UTC)),
			datetime!(2026-01-06 22:00 UTC)
		);
		let at = datetime!(2026-01-06 01:00 UTC);
		assert_eq!(night.next_open(at), at);
		// 02:00 to 03:00 at UTC+02:00 is 00:00 to 01:00 UTC.
		let east = Window {
			start: 2 * 60,
			end: 3 * 60,
			offset: 120,
		};
		assert!(east.open_at(datetime!(2026-01-06 00:30 UTC)));
		assert_eq!(
			east.next_open(datetime!(2026-01-06 01:00 UTC)),
			datetime!(2026-01-07 00:00 UTC)
		);
	}

	#[test]
	fn the_next_check_follows_the_interval_and_the_window() {
		let now = datetime!(2026-01-06 10:00 UTC);
		assert_eq!(
			next_check(now, Interval::Hourly.every(), None),
			datetime!(2026-01-06 11:00 UTC)
		);
		let window = Window {
			start: 2 * 60,
			end: 4 * 60,
			offset: 0,
		};
		assert_eq!(
			next_check(now, Interval::Hourly.every(), Some(window)),
			datetime!(2026-01-07 02:00 UTC)
		);
		// Daily from inside the window lands inside it again.
		let inside = datetime!(2026-01-06 02:05 UTC);
		assert_eq!(
			next_check(inside, Interval::Daily.every(), Some(window)),
			datetime!(2026-01-07 02:05 UTC)
		);
	}

	#[test]
	fn waiting_retries_sooner_than_settling() {
		assert_eq!(Checked::UpToDate.delay(Interval::Daily), Duration::days(1));
		assert_eq!(
			Checked::SkippedBad.delay(Interval::Hourly),
			Duration::hours(1)
		);
		assert_eq!(Checked::Frozen.delay(Interval::Daily), RETRY);
		assert_eq!(Checked::Busy.delay(Interval::Daily), RETRY);
		assert_eq!(Checked::Error.delay(Interval::Daily), ERROR_RETRY);
		assert_eq!(Checked::Error.delay(Interval::Hourly), Duration::hours(1));
		for c in Checked::ALL {
			assert_eq!(Checked::from_stored(c.as_str()), Some(c));
		}
	}

	#[test]
	fn times_and_digests_read_and_print() {
		assert_eq!(parse_time("02:30"), Some(150));
		assert_eq!(parse_time("2:05"), Some(125));
		assert_eq!(parse_time("24:00"), None);
		assert_eq!(parse_time("ab"), None);
		assert_eq!(time_label(150), "02:30");
		assert!(trackable("ghcr.io/me/app:latest").is_ok());
		assert!(trackable("ghcr.io/me/app@sha256:abc").is_err());
		assert_eq!(digest_of("ghcr.io/me/app:latest"), None);
		assert_eq!(short_digest(&d('a')), "sha256:aaaaaaaaaaaa");
	}
}
