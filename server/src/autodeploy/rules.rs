//! The decisions that need no database and no network: which refs and files a push counts for,
//! whether its message asks to be skipped, whether a freeze window is open, and what a set of
//! check results adds up to. Everything here is a function of its arguments, so the tests give
//! it the clock.

use time::{Duration, OffsetDateTime};

/// Whether `text` matches the glob `pattern`. `*` and `?` stay inside one path segment, `**`
/// crosses segments, and a leading `**/` also matches nothing at all (`**/a.rs` matches `a.rs`).
#[must_use]
#[allow(
	clippy::many_single_char_names,
	reason = "a small tokenizer over bytes"
)]
pub fn glob_match(pattern: &str, text: &str) -> bool {
	#[derive(Clone, Copy)]
	enum Tok {
		Lit(u8),
		One,
		Star,
		Deep,
		DirStar,
	}
	let p = pattern.as_bytes();
	let mut toks = Vec::new();
	let mut i = 0;
	while i < p.len() {
		if p[i..].starts_with(b"**/") {
			toks.push(Tok::DirStar);
			i += 3;
		} else if p[i..].starts_with(b"**") {
			toks.push(Tok::Deep);
			i += 2;
		} else {
			toks.push(match p[i] {
				b'*' => Tok::Star,
				b'?' => Tok::One,
				c => Tok::Lit(c),
			});
			i += 1;
		}
	}
	let t = text.as_bytes();
	let n = t.len();
	// ok[i][j]: toks[i..] matches t[j..].
	let mut ok = vec![vec![false; n + 1]; toks.len() + 1];
	ok[toks.len()][n] = true;
	for i in (0..toks.len()).rev() {
		for j in (0..=n).rev() {
			ok[i][j] = match toks[i] {
				Tok::Lit(c) => j < n && t[j] == c && ok[i + 1][j + 1],
				Tok::One => j < n && t[j] != b'/' && ok[i + 1][j + 1],
				Tok::Star => ok[i + 1][j] || (j < n && t[j] != b'/' && ok[i][j + 1]),
				Tok::Deep => ok[i + 1][j] || (j < n && ok[i][j + 1]),
				Tok::DirStar => {
					ok[i + 1][j] || (j + 1..=n).any(|k| t[k - 1] == b'/' && ok[i + 1][k])
				}
			};
		}
	}
	ok[0][0]
}

/// Whether the ref `name` (a branch or tag name, without `refs/heads/`) is one of `patterns`.
#[must_use]
pub fn ref_matches(patterns: &[String], name: &str) -> bool {
	patterns.iter().any(|p| glob_match(p.trim(), name))
}

/// A path pattern the way people write them: no slash means any depth (`*.md` is every
/// Markdown file), a trailing slash means everything below.
fn path_glob(pattern: &str, path: &str) -> bool {
	let pattern = pattern.trim().trim_start_matches("./");
	let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
	if let Some(dir) = pattern.strip_suffix('/') {
		return glob_match(&format!("{dir}/**"), path);
	}
	if pattern.contains('/') {
		glob_match(pattern, path) || glob_match(&format!("{pattern}/**"), path)
	} else {
		glob_match(&format!("**/{pattern}"), path) || glob_match(&format!("{pattern}/**"), path)
	}
}

/// Whether a push that changed `files` deploys under `filters` (`api/**`, `!docs/**`): at
/// least one file must match an include (any file, when there are none) and no exclude.
/// No filters, or no known files, deploys: an unknown list must never silently stop a deploy.
#[must_use]
pub fn paths_pass(filters: &[String], files: Option<&[String]>) -> bool {
	let filters: Vec<&str> = filters
		.iter()
		.map(|f| f.trim())
		.filter(|f| !f.is_empty())
		.collect();
	let Some(files) = files.filter(|f| !f.is_empty()) else {
		return true;
	};
	if filters.is_empty() {
		return true;
	}
	let includes: Vec<&str> = filters
		.iter()
		.filter(|f| !f.starts_with('!'))
		.copied()
		.collect();
	let excludes: Vec<&str> = filters.iter().filter_map(|f| f.strip_prefix('!')).collect();
	files.iter().any(|file| {
		(includes.is_empty() || includes.iter().any(|p| path_glob(p, file)))
			&& !excludes.iter().any(|p| path_glob(p, file))
	})
}

/// The marker in a commit message that asks for no deploy, if any.
#[must_use]
pub fn skip_marker(message: &str) -> Option<&'static str> {
	let lower = message.to_lowercase();
	["[skip deploy]", "[skip ci]", "[ci skip]"]
		.into_iter()
		.find(|m| lower.contains(m))
}

const MINUTES_PER_WEEK: i64 = 7 * 24 * 60;

/// One freeze window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Window {
	/// Every week from `start` to `end`, in minutes since Monday 00:00 of a clock `offset`
	/// minutes ahead of UTC. `end` before `start` wraps over the weekend.
	Recurring { start: i64, end: i64, offset: i64 },
	/// One span of time.
	Once {
		from: OffsetDateTime,
		to: OffsetDateTime,
	},
}

impl Window {
	/// Whether the window is open at `now`.
	#[must_use]
	pub fn open_at(&self, now: OffsetDateTime) -> bool {
		match *self {
			Self::Once { from, to } => from <= now && now < to,
			Self::Recurring { start, end, offset } => {
				let local = now + Duration::minutes(offset);
				let minute = i64::from(local.weekday().number_days_from_monday()) * 24 * 60
					+ i64::from(local.hour()) * 60
					+ i64::from(local.minute());
				let minute = minute.rem_euclid(MINUTES_PER_WEEK);
				if start < end {
					start <= minute && minute < end
				} else {
					minute >= start || minute < end
				}
			}
		}
	}
}

/// Whether any window is open at `now`.
#[must_use]
pub fn frozen_at(windows: &[Window], now: OffsetDateTime) -> bool {
	windows.iter().any(|w| w.open_at(now))
}

/// `Fri 16:00` for a minute of the week.
#[must_use]
pub fn minute_label(minute: i64) -> String {
	const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
	let minute = minute.rem_euclid(MINUTES_PER_WEEK);
	format!(
		"{} {:02}:{:02}",
		DAYS[usize::try_from(minute / (24 * 60)).unwrap_or(0) % 7],
		minute % (24 * 60) / 60,
		minute % 60
	)
}

/// The minute of the week for a day (0 is Monday) and a clock time.
#[must_use]
pub const fn minute_of_week(day: i64, hour: i64, minute: i64) -> i64 {
	day * 24 * 60 + hour * 60 + minute
}

/// What a commit's checks add up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ci {
	/// Every check finished well.
	Pass,
	/// A check failed; the text names it.
	Fail(String),
	/// Something is still running or queued.
	Pending,
	/// The provider knows no checks for the commit (yet).
	NoChecks,
}

/// One check's result, before it is added up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
	Ok,
	Failed,
	Running,
}

/// Adds checks up: a failure wins, then anything still running, then success; no checks at
/// all is its own answer.
#[must_use]
pub fn combine(checks: &[(String, Check)]) -> Ci {
	if let Some((name, _)) = checks.iter().find(|(_, c)| *c == Check::Failed) {
		return Ci::Fail(name.clone());
	}
	if checks.iter().any(|(_, c)| *c == Check::Running) {
		return Ci::Pending;
	}
	if checks.is_empty() {
		Ci::NoChecks
	} else {
		Ci::Pass
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use time::macros::datetime;

	fn s(v: &[&str]) -> Vec<String> {
		v.iter().map(|x| (*x).to_owned()).collect()
	}

	#[test]
	fn autodeploy_glob_stars_stay_in_a_segment_and_double_stars_do_not() {
		assert!(glob_match("main", "main"));
		assert!(!glob_match("main", "mainline"));
		assert!(glob_match("release/*", "release/1.2"));
		assert!(!glob_match("release/*", "release/1/2"));
		assert!(glob_match("release/**", "release/1/2"));
		assert!(glob_match("v?.*", "v1.2"));
		assert!(glob_match("**/a.rs", "a.rs"));
		assert!(glob_match("**/a.rs", "x/y/a.rs"));
		assert!(!glob_match("**/a.rs", "x/ba.rs"));
		assert!(glob_match("*", "main"));
		assert!(!glob_match("*", "feature/x"));
		assert!(glob_match("**", "feature/x"));
		assert!(!glob_match("", "x"));
		assert!(glob_match("", ""));
	}

	#[test]
	fn autodeploy_branch_patterns_match_any_listed() {
		let p = s(&["main", "release/*"]);
		assert!(ref_matches(&p, "main"));
		assert!(ref_matches(&p, "release/2"));
		assert!(!ref_matches(&p, "feature/x"));
		assert!(!ref_matches(&[], "main"));
	}

	#[test]
	fn autodeploy_path_filters_include_exclude_and_fail_open() {
		let files = |v: &[&str]| Some(s(v));
		let f = s(&["api/**", "!docs/**"]);
		assert!(paths_pass(&f, files(&["api/src/main.rs"]).as_deref()));
		assert!(!paths_pass(&f, files(&["web/app.js"]).as_deref()));
		assert!(!paths_pass(&f, files(&["docs/readme.md"]).as_deref()));
		assert!(paths_pass(
			&f,
			files(&["docs/readme.md", "api/x.rs"]).as_deref()
		));
		// Only excludes: everything but the excluded deploys.
		let only_ex = s(&["!docs/**", "!*.md"]);
		assert!(paths_pass(&only_ex, files(&["src/a.rs"]).as_deref()));
		assert!(!paths_pass(
			&only_ex,
			files(&["docs/a.rs", "README.md", "sub/NOTES.md"]).as_deref()
		));
		// A bare name matches at any depth; a directory name matches what is under it.
		assert!(paths_pass(
			&s(&["Dockerfile"]),
			files(&["svc/Dockerfile"]).as_deref()
		));
		assert!(paths_pass(&s(&["api"]), files(&["api/x/y.rs"]).as_deref()));
		assert!(paths_pass(&s(&["api/"]), files(&["api/x.rs"]).as_deref()));
		// No filters, an unknown list and an empty one all deploy.
		assert!(paths_pass(&[], files(&["x"]).as_deref()));
		assert!(paths_pass(&f, None));
		assert!(paths_pass(&f, Some(&[])));
		// Blank lines are no filter at all.
		assert!(paths_pass(&s(&["  "]), files(&["x"]).as_deref()));
	}

	#[test]
	fn autodeploy_skip_markers_in_any_case() {
		assert_eq!(skip_marker("fix typo [skip deploy]"), Some("[skip deploy]"));
		assert_eq!(skip_marker("wip [SKIP CI]"), Some("[skip ci]"));
		assert_eq!(skip_marker("wip [ci skip]\n\nbody"), Some("[ci skip]"));
		assert_eq!(skip_marker("skip deploy"), None);
		assert_eq!(skip_marker(""), None);
	}

	#[test]
	fn autodeploy_recurring_freeze_wraps_the_weekend() {
		// Friday 16:00 to Monday 08:00.
		let w = Window::Recurring {
			start: minute_of_week(4, 16, 0),
			end: minute_of_week(0, 8, 0),
			offset: 0,
		};
		// 2026-11-20 is a Friday.
		assert!(!w.open_at(datetime!(2026-11-20 15:59 UTC)));
		assert!(w.open_at(datetime!(2026-11-20 16:00 UTC)));
		assert!(w.open_at(datetime!(2026-11-21 12:00 UTC)));
		assert!(w.open_at(datetime!(2026-11-22 23:59 UTC)));
		assert!(w.open_at(datetime!(2026-11-23 07:59 UTC)));
		assert!(!w.open_at(datetime!(2026-11-23 08:00 UTC)));
		assert!(!w.open_at(datetime!(2026-11-25 12:00 UTC)));
		// The same window on a clock two hours ahead opens two hours earlier in UTC.
		let ahead = Window::Recurring {
			start: minute_of_week(4, 16, 0),
			end: minute_of_week(0, 8, 0),
			offset: 120,
		};
		assert!(ahead.open_at(datetime!(2026-11-20 14:00 UTC)));
		assert!(!ahead.open_at(datetime!(2026-11-20 13:59 UTC)));
		// A window inside one week does not wrap.
		let mid = Window::Recurring {
			start: minute_of_week(2, 9, 0),
			end: minute_of_week(2, 17, 0),
			offset: 0,
		};
		assert!(mid.open_at(datetime!(2026-11-18 12:00 UTC)));
		assert!(!mid.open_at(datetime!(2026-11-19 12:00 UTC)));
	}

	#[test]
	fn autodeploy_one_off_freeze_is_half_open() {
		let w = Window::Once {
			from: datetime!(2026-12-24 00:00 UTC),
			to: datetime!(2026-12-27 00:00 UTC),
		};
		assert!(!w.open_at(datetime!(2026-12-23 23:59 UTC)));
		assert!(w.open_at(datetime!(2026-12-24 00:00 UTC)));
		assert!(!w.open_at(datetime!(2026-12-27 00:00 UTC)));
		assert!(frozen_at(&[w], datetime!(2026-12-25 10:00 UTC)));
		assert!(!frozen_at(&[], datetime!(2026-12-25 10:00 UTC)));
	}

	#[test]
	fn autodeploy_minute_labels() {
		assert_eq!(minute_label(minute_of_week(4, 16, 0)), "Fri 16:00");
		assert_eq!(minute_label(minute_of_week(0, 8, 5)), "Mon 08:05");
	}

	#[test]
	fn autodeploy_checks_add_up_failure_first() {
		let c = |v: &[(&str, Check)]| {
			combine(
				&v.iter()
					.map(|(n, c)| ((*n).to_owned(), *c))
					.collect::<Vec<_>>(),
			)
		};
		assert_eq!(c(&[]), Ci::NoChecks);
		assert_eq!(c(&[("a", Check::Ok), ("b", Check::Ok)]), Ci::Pass);
		assert_eq!(c(&[("a", Check::Ok), ("b", Check::Running)]), Ci::Pending);
		assert_eq!(
			c(&[("a", Check::Running), ("b", Check::Failed)]),
			Ci::Fail("b".to_owned())
		);
	}
}
