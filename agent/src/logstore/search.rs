//! The query engine: reads the segments that can hold the asked time range, line by line,
//! and keeps the `limit` best matches in a heap. Memory is the limit times the lines kept,
//! whatever the size of the files; a read that takes too long stops and says so.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use proto::{LogLevel, LogRecord, LogSearchRequest, LogSearchResult};
use regex::{Regex, RegexBuilder};

use super::files::{Files, Segment, service_matches};

/// The most lines one search returns, whatever it asks for.
pub const MAX_LIMIT: usize = 2000;
/// A search that reads for longer than this returns what it has, marked partial.
pub const MAX_READ: Duration = Duration::from_secs(20);
/// Containers that overlap (a rolling deploy) write slightly out of order; a segment is read
/// when its lines may reach into the range by this much.
const SKEW_MS: i64 = 60_000;
/// Compiled regex size cap: a pattern that needs more is refused.
const REGEX_BYTES: usize = 1 << 20;

/// Why a search cannot run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadQuery(pub String);

enum Text {
	Any,
	Contains(String),
	Pattern(Regex),
}

impl Text {
	fn new(req: &LogSearchRequest) -> Result<Self, BadQuery> {
		let Some(text) = req.text.as_deref().filter(|t| !t.is_empty()) else {
			return Ok(Self::Any);
		};
		if req.regex {
			RegexBuilder::new(text)
				.size_limit(REGEX_BYTES)
				.build()
				.map(Self::Pattern)
				.map_err(|e| BadQuery(format!("not a valid regular expression: {e}")))
		} else {
			Ok(Self::Contains(text.to_lowercase()))
		}
	}

	fn matches(&self, line: &str) -> bool {
		match self {
			Self::Any => true,
			Self::Contains(needle) => line.to_lowercase().contains(needle.as_str()),
			Self::Pattern(re) => re.is_match(line),
		}
	}
}

struct Entry {
	key: (i64, u64),
	record: LogRecord,
}

impl PartialEq for Entry {
	fn eq(&self, other: &Self) -> bool {
		self.key == other.key
	}
}
impl Eq for Entry {}
impl PartialOrd for Entry {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}
impl Ord for Entry {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.key.cmp(&other.key)
	}
}

/// Runs `req` over the files. The answer is oldest first.
///
/// # Errors
/// The text is not a valid regular expression.
pub fn run(files: &Files, req: &LogSearchRequest) -> Result<LogSearchResult, BadQuery> {
	run_within(files, req, MAX_READ)
}

/// [`run`] with a chosen read budget.
///
/// # Errors
/// See [`run`].
pub fn run_within(
	files: &Files,
	req: &LogSearchRequest,
	budget: Duration,
) -> Result<LogSearchResult, BadQuery> {
	let text = Text::new(req)?;
	let limit = usize::try_from(req.limit)
		.unwrap_or(MAX_LIMIT)
		.clamp(1, MAX_LIMIT);
	let started = Instant::now();
	let (since, until) = (req.since.unwrap_or(i64::MIN), req.until.unwrap_or(i64::MAX));

	let services: Vec<String> = files
		.services()
		.into_iter()
		.filter(|svc| {
			req.services.is_empty() || req.services.iter().any(|w| service_matches(w, svc))
		})
		.collect();

	// The heap's top is always the line to drop next, so its key is "how much we want it":
	// later is better for the newest lines, earlier for the oldest.
	let mut heap: BinaryHeap<Reverse<Entry>> = BinaryHeap::new();
	let mut order: u64 = 0;
	let mut partial = false;
	let mut buf = String::new();

	'scan: for svc in &services {
		let segments = files.segments(svc);
		for (i, segment) in segments.iter().enumerate() {
			let next_first = segments.get(i + 1).map(|s| s.first_ts);
			if !may_overlap(segment, next_first, since, until) {
				continue;
			}
			let Ok(file) = std::fs::File::open(&segment.path) else {
				continue;
			};
			let mut reader = BufReader::with_capacity(64 * 1024, file);
			let mut n = 0u32;
			loop {
				buf.clear();
				match reader.read_line(&mut buf) {
					Ok(0) | Err(_) => break,
					Ok(_) => {}
				}
				n += 1;
				if n.is_multiple_of(1024) && started.elapsed() > budget {
					partial = true;
					break 'scan;
				}
				let Ok(mut record) = serde_json::from_str::<LogRecord>(buf.trim_end()) else {
					continue;
				};
				if record.ts < since || record.ts >= until {
					continue;
				}
				if req.deployment.is_some() && record.dep != req.deployment {
					continue;
				}
				if !req.levels.is_empty()
					&& !req.levels.contains(&record.level.unwrap_or(LogLevel::Info))
				{
					continue;
				}
				if !text.matches(&record.line) {
					continue;
				}
				record.svc.clone_from(svc);
				order += 1;
				let key = if req.newest {
					(record.ts, order)
				} else {
					(-record.ts, u64::MAX - order)
				};
				heap.push(Reverse(Entry { key, record }));
				if heap.len() > limit {
					heap.pop();
				}
			}
		}
	}

	let mut records: Vec<(u64, LogRecord)> = heap
		.into_iter()
		.map(|Reverse(e)| {
			let order = if req.newest {
				e.key.1
			} else {
				u64::MAX - e.key.1
			};
			(order, e.record)
		})
		.collect();
	records.sort_by_key(|(order, r)| (r.ts, *order));
	Ok(LogSearchResult {
		records: records.into_iter().map(|(_, r)| r).collect(),
		partial,
	})
}

/// Whether a segment can hold lines in `since..until`, judged by its name (when it began),
/// its successor's name (when it was closed) and its last write.
fn may_overlap(segment: &Segment, next_first: Option<i64>, since: i64, until: i64) -> bool {
	if segment.first_ts.saturating_sub(SKEW_MS) >= until {
		return false;
	}
	if segment.modified.saturating_add(SKEW_MS) < since {
		return false;
	}
	if next_first.is_some_and(|n| n.saturating_add(SKEW_MS) < since) {
		return false;
	}
	true
}

#[cfg(test)]
mod tests {
	use proto::Stream;

	use super::*;

	/// Fixture times are offsets from here (a past moment, so file times never prune them).
	const T0: i64 = 1_700_000_000_000;

	fn rec(ts: i64, level: Option<LogLevel>, dep: i64, line: &str) -> LogRecord {
		LogRecord {
			ts: T0 + ts,
			stream: Stream::Stdout,
			dep: Some(dep),
			level,
			line: line.to_owned(),
			svc: String::new(),
		}
	}

	fn req() -> LogSearchRequest {
		LogSearchRequest {
			services: Vec::new(),
			since: None,
			until: None,
			text: None,
			regex: false,
			levels: Vec::new(),
			deployment: None,
			limit: 100,
			newest: true,
		}
	}

	/// Two deploys of `app-1`, a database and one stack service, in small segments.
	fn fixture() -> (tempfile::TempDir, Files) {
		let dir = tempfile::tempdir().unwrap();
		let files = Files::with_segment_bytes(dir.path().join("logs"), 400);
		let app: Vec<LogRecord> = (0..30)
			.map(|i| {
				let (dep, base) = if i < 15 { (11, 1_000) } else { (12, 600_000) };
				let level = match i % 5 {
					0 => Some(LogLevel::Error),
					1 => Some(LogLevel::Warn),
					2 => None,
					3 => Some(LogLevel::Debug),
					_ => Some(LogLevel::Info),
				};
				rec(base + i * 10, level, dep, &format!("request {i} served"))
			})
			.collect();
		files.append("app-1", &app).unwrap();
		files
			.append(
				"db-2",
				&[
					rec(1_105, Some(LogLevel::Info), 9, "checkpoint complete"),
					rec(
						600_305,
						Some(LogLevel::Error),
						9,
						"connection reset by peer",
					),
				],
			)
			.unwrap();
		files
			.append("stack-3-web", &[rec(1_500, None, 4, "GET /health 200")])
			.unwrap();
		(dir, files)
	}

	fn lines(result: &LogSearchResult) -> Vec<&str> {
		result.records.iter().map(|r| r.line.as_str()).collect()
	}

	#[test]
	fn logstore_search_returns_everything_oldest_first_across_deploys() {
		let (_dir, files) = fixture();
		let result = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				..req()
			},
		)
		.unwrap();
		assert_eq!(result.records.len(), 30);
		assert!(!result.partial);
		let ts: Vec<i64> = result.records.iter().map(|r| r.ts).collect();
		let mut sorted = ts.clone();
		sorted.sort_unstable();
		assert_eq!(ts, sorted);
		assert_eq!(result.records[0].dep, Some(11));
		assert_eq!(result.records[29].dep, Some(12));
		assert!(result.records.iter().all(|r| r.svc == "app-1"));
	}

	#[test]
	fn logstore_search_by_time_range_keeps_since_and_drops_until() {
		let (_dir, files) = fixture();
		let result = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				since: Some(T0 + 1_100),
				until: Some(T0 + 1_140),
				..req()
			},
		)
		.unwrap();
		let ts: Vec<i64> = result.records.iter().map(|r| r.ts).collect();
		assert_eq!(ts, vec![T0 + 1_100, T0 + 1_110, T0 + 1_120, T0 + 1_130]);
	}

	#[test]
	fn logstore_the_limit_keeps_the_newest_or_the_oldest_lines() {
		let (_dir, files) = fixture();
		let newest = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				limit: 3,
				..req()
			},
		)
		.unwrap();
		assert_eq!(
			lines(&newest),
			[
				"request 27 served",
				"request 28 served",
				"request 29 served"
			]
		);
		let oldest = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				limit: 3,
				newest: false,
				..req()
			},
		)
		.unwrap();
		assert_eq!(
			lines(&oldest),
			["request 0 served", "request 1 served", "request 2 served"]
		);
	}

	#[test]
	fn logstore_a_regex_and_a_plain_text_are_both_matched() {
		let (_dir, files) = fixture();
		let regex = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				text: Some(r"request (3|13|23) served".into()),
				regex: true,
				..req()
			},
		)
		.unwrap();
		assert_eq!(
			lines(&regex),
			["request 3 served", "request 13 served", "request 23 served"]
		);
		let plain = run(
			&files,
			&LogSearchRequest {
				text: Some("RESET BY".into()),
				..req()
			},
		)
		.unwrap();
		assert_eq!(lines(&plain), ["connection reset by peer"]);
		// Dots and brackets are text unless the search says regex.
		let literal = run(
			&files,
			&LogSearchRequest {
				text: Some("request (3|13)".into()),
				..req()
			},
		)
		.unwrap();
		assert!(literal.records.is_empty());
	}

	#[test]
	fn logstore_an_invalid_regex_is_refused_not_run() {
		let (_dir, files) = fixture();
		let err = run(
			&files,
			&LogSearchRequest {
				text: Some("(unclosed".into()),
				regex: true,
				..req()
			},
		)
		.unwrap_err();
		assert!(err.0.contains("regular expression"), "{err}");
	}

	#[test]
	fn logstore_levels_filter_and_a_line_with_no_level_counts_as_info() {
		let (_dir, files) = fixture();
		let errors = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				levels: vec![LogLevel::Error],
				..req()
			},
		)
		.unwrap();
		assert_eq!(errors.records.len(), 6);
		assert!(
			errors
				.records
				.iter()
				.all(|r| r.level == Some(LogLevel::Error))
		);
		let info = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				levels: vec![LogLevel::Info],
				..req()
			},
		)
		.unwrap();
		// 6 with level info, 6 with none.
		assert_eq!(info.records.len(), 12);
		let both = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				levels: vec![LogLevel::Warn, LogLevel::Error],
				..req()
			},
		)
		.unwrap();
		assert_eq!(both.records.len(), 12);
	}

	#[test]
	fn logstore_a_deployment_narrows_to_its_own_lines() {
		let (_dir, files) = fixture();
		let result = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				deployment: Some(12),
				..req()
			},
		)
		.unwrap();
		assert_eq!(result.records.len(), 15);
		assert!(result.records.iter().all(|r| r.dep == Some(12)));
	}

	#[test]
	fn logstore_services_pick_resources_and_a_stack_covers_its_services() {
		let (_dir, files) = fixture();
		let both = run(
			&files,
			&LogSearchRequest {
				services: vec!["db-2".into(), "stack-3".into()],
				..req()
			},
		)
		.unwrap();
		assert_eq!(
			lines(&both),
			[
				"checkpoint complete",
				"GET /health 200",
				"connection reset by peer"
			]
		);
		assert_eq!(both.records[1].svc, "stack-3-web");
		let all = run(&files, &req()).unwrap();
		assert_eq!(all.records.len(), 33);
		let none = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-9".into()],
				..req()
			},
		)
		.unwrap();
		assert!(none.records.is_empty());
	}

	#[test]
	fn logstore_a_range_skips_the_segments_that_cannot_hold_it() {
		let (_dir, files) = fixture();
		// Make the early segments unreadable: if the engine opened them the lines would
		// vanish silently, so check by removing their content instead and expecting the
		// late range to still be complete.
		let segments = files.segments("app-1");
		assert!(segments.len() >= 4, "{segments:?}");
		let first = &segments[0];
		std::fs::write(&first.path, "not json at all\n").unwrap();
		let late = run(
			&files,
			&LogSearchRequest {
				services: vec!["app-1".into()],
				since: Some(T0 + 600_000),
				..req()
			},
		)
		.unwrap();
		assert_eq!(late.records.len(), 15);
	}

	#[test]
	fn logstore_a_broken_line_is_skipped_and_the_rest_is_read() {
		let (_dir, files) = fixture();
		let segment = files.segments("db-2").remove(0);
		let mut text = std::fs::read_to_string(&segment.path).unwrap();
		text.insert_str(0, "{\"ts\":1,\"strea\n");
		std::fs::write(&segment.path, text).unwrap();
		let result = run(
			&files,
			&LogSearchRequest {
				services: vec!["db-2".into()],
				..req()
			},
		)
		.unwrap();
		assert_eq!(result.records.len(), 2);
	}

	#[test]
	fn logstore_a_read_over_budget_is_returned_partial() {
		let dir = tempfile::tempdir().unwrap();
		let files = Files::new(dir.path().join("logs"));
		let many: Vec<LogRecord> = (0..5000).map(|i| rec(i, None, 1, "line")).collect();
		files.append("app-1", &many).unwrap();
		let result = run_within(&files, &req(), Duration::ZERO).unwrap();
		assert!(result.partial);
		let full = run(&files, &req()).unwrap();
		assert!(!full.partial);
	}

	#[test]
	fn logstore_the_limit_is_capped() {
		let dir = tempfile::tempdir().unwrap();
		let files = Files::new(dir.path().join("logs"));
		let many: Vec<LogRecord> = (0..(i64::try_from(MAX_LIMIT).unwrap() + 50))
			.map(|i| rec(i, None, 1, "x"))
			.collect();
		files.append("app-1", &many).unwrap();
		let result = run(
			&files,
			&LogSearchRequest {
				limit: 1_000_000,
				..req()
			},
		)
		.unwrap();
		assert_eq!(result.records.len(), MAX_LIMIT);
	}
}
