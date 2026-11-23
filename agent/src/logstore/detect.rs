//! What a raw container log line says about itself: when it was written and how serious it
//! is. Podman stamps every line (`timestamps=true`); the level comes from the line only when
//! it names one, as a JSON field, a `level=` pair or a leading word. A guess from words in
//! the middle of the text would colour `0 errors` red, so there is none.

use proto::LogLevel;

/// Longest prefix of a line searched for a level word.
const PREFIX_SCAN: usize = 96;

/// Splits Podman's `2026-11-21T08:00:00.123456789Z text` into the time (unix milliseconds)
/// and the text. A line without a readable stamp comes back whole, with `None`.
#[must_use]
pub fn split_timestamp(raw: &str) -> (Option<i64>, &str) {
	let Some((stamp, rest)) = raw.split_once(' ') else {
		return match parse_rfc3339_ms(raw) {
			Some(ts) => (Some(ts), ""),
			None => (None, raw),
		};
	};
	match parse_rfc3339_ms(stamp) {
		Some(ts) => (Some(ts), rest),
		None => (None, raw),
	}
}

/// `2026-11-21T08:00:00.123456789Z` or with a `+hh:mm` offset, as unix milliseconds.
#[must_use]
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
	let raw = s.as_bytes();
	if raw.len() < 20 || raw[4] != b'-' || raw[7] != b'-' || !matches!(raw[10], b'T' | b't' | b' ')
	{
		return None;
	}
	let num = |range: std::ops::Range<usize>| -> Option<i64> {
		let part = s.get(range)?;
		part.bytes()
			.all(|c| c.is_ascii_digit())
			.then(|| part.parse().ok())?
	};
	let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
	let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
	if raw[13] != b':' || raw[16] != b':' {
		return None;
	}
	if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
		return None;
	}
	if second > 60 {
		return None;
	}
	let mut pos = 19;
	let mut millis = 0i64;
	if raw.get(pos) == Some(&b'.') {
		pos += 1;
		let start = pos;
		while raw.get(pos).is_some_and(u8::is_ascii_digit) {
			pos += 1;
		}
		if pos == start {
			return None;
		}
		let digits = &s[start..pos];
		let first3 = format!("{digits:0<3}");
		millis = first3[..3].parse().ok()?;
	}
	let offset_secs = match raw.get(pos)? {
		b'Z' | b'z' if pos + 1 == raw.len() => 0,
		sign @ (b'+' | b'-') => {
			let rest = s.get(pos + 1..)?;
			let (hours, mins) = rest.split_once(':')?;
			let secs = hours.parse::<i64>().ok()? * 3600 + mins.parse::<i64>().ok()? * 60;
			if *sign == b'+' { secs } else { -secs }
		}
		_ => return None,
	};
	let days = days_from_civil(year, month, day);
	let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
	Some(secs * 1000 + millis)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
	let y = if month <= 2 { year - 1 } else { year };
	let era = y.div_euclid(400);
	let yoe = y - era * 400;
	let mp = (month + 9) % 12;
	let doy = (153 * mp + 2) / 5 + day - 1;
	let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	era * 146_097 + doe - 719_468
}

/// The level `line` names, if it names one.
#[must_use]
pub fn level_of(line: &str) -> Option<LogLevel> {
	let trimmed = line.trim_start();
	if trimmed.starts_with('{') {
		return json_level(trimmed);
	}
	logfmt_level(trimmed).or_else(|| prefix_level(trimmed))
}

fn word_level(word: &str) -> Option<LogLevel> {
	match word.to_ascii_lowercase().as_str() {
		"trace" | "trc" | "debug" | "dbg" | "dbug" | "verbose" => Some(LogLevel::Debug),
		"info" | "inf" | "information" | "notice" => Some(LogLevel::Info),
		"warn" | "wrn" | "warning" => Some(LogLevel::Warn),
		"error" | "err" | "fatal" | "crit" | "critical" | "panic" | "emerg" | "alert"
		| "severe" => Some(LogLevel::Error),
		_ => None,
	}
}

/// The JSON loggers' own numbers: pino and bunyan (10 to 60) and syslog-ish 0 to 7.
fn number_level(n: f64) -> Option<LogLevel> {
	match n {
		n if (10.0..=60.0).contains(&n) => Some(match n {
			n if n < 30.0 => LogLevel::Debug,
			n if n < 40.0 => LogLevel::Info,
			n if n < 50.0 => LogLevel::Warn,
			_ => LogLevel::Error,
		}),
		_ => None,
	}
}

fn json_level(line: &str) -> Option<LogLevel> {
	// Beyond this a line is not a log record anyone parses for a level.
	if line.len() > 8 * 1024 {
		return None;
	}
	let value: serde_json::Value = serde_json::from_str(line).ok()?;
	let object = value.as_object()?;
	for key in [
		"level",
		"lvl",
		"severity",
		"levelname",
		"log.level",
		"severity_text",
		"log_level",
		"loglevel",
	] {
		match object.get(key) {
			Some(serde_json::Value::String(s)) => {
				if let Some(level) = word_level(s) {
					return Some(level);
				}
			}
			Some(serde_json::Value::Number(n)) => {
				if let Some(level) = n.as_f64().and_then(number_level) {
					return Some(level);
				}
			}
			_ => {}
		}
	}
	None
}

fn logfmt_level(line: &str) -> Option<LogLevel> {
	let head = head(line, 400);
	for token in head.split_whitespace() {
		let Some((key, value)) = token.split_once('=') else {
			continue;
		};
		if matches!(key, "level" | "lvl" | "severity") {
			return word_level(value.trim_matches('"'));
		}
	}
	None
}

fn head(s: &str, max: usize) -> &str {
	if s.len() <= max {
		return s;
	}
	let mut end = max;
	while !s.is_char_boundary(end) {
		end -= 1;
	}
	&s[..end]
}

/// A leading word: `[ERROR]`, `ERROR:`, `WARN `, `ERROR:root:text`, klog's `E0102 12:00`.
/// Only the first few tokens count, and only when written as a marker (brackets, a colon, or
/// capitals), so a sentence that merely starts with "error" is left alone.
fn prefix_level(line: &str) -> Option<LogLevel> {
	let scan = head(line, PREFIX_SCAN);
	for (i, token) in scan.split_whitespace().take(4).enumerate() {
		if let Some(level) = klog_level(token) {
			return Some(level);
		}
		let bracketed = token.starts_with(['[', '(', '<']);
		let inner =
			token.trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | '<' | '>' | '|'));
		let (word, colon) = match inner.split_once(':') {
			Some((word, _)) => (word, true),
			None => (inner, false),
		};
		let upper = word.len() > 1 && word.bytes().all(|b| b.is_ascii_uppercase());
		if (bracketed || upper || (colon && i == 0))
			&& let Some(level) = word_level(word)
		{
			return Some(level);
		}
	}
	None
}

/// `E0102`, `W1231`: Kubernetes-style klog and glog.
fn klog_level(token: &str) -> Option<LogLevel> {
	let b = token.as_bytes();
	if b.len() != 5 || !b[1..].iter().all(u8::is_ascii_digit) {
		return None;
	}
	match b[0] {
		b'E' | b'F' => Some(LogLevel::Error),
		b'W' => Some(LogLevel::Warn),
		b'I' => Some(LogLevel::Info),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn logstore_a_podman_timestamp_becomes_milliseconds() {
		assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
		assert_eq!(
			parse_rfc3339_ms("2026-11-21T08:00:00.123456789Z"),
			Some(1_795_248_000_123)
		);
		assert_eq!(
			parse_rfc3339_ms("2026-11-21T10:00:00.5+02:00"),
			Some(1_795_248_000_500)
		);
		assert_eq!(
			parse_rfc3339_ms("2024-02-29T23:59:59Z"),
			Some(1_709_251_199_000)
		);
		for bad in ["", "hello", "2026-13-01T00:00:00Z", "2026-11-21T08:00:00"] {
			assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
		}
	}

	#[test]
	fn logstore_the_stamp_is_split_off_the_text() {
		assert_eq!(
			split_timestamp("2026-11-21T08:00:00.1Z hello world"),
			(Some(1_795_248_000_100), "hello world")
		);
		assert_eq!(split_timestamp("no stamp here"), (None, "no stamp here"));
		assert_eq!(
			split_timestamp("2026-11-21T08:00:00Z"),
			(Some(1_795_248_000_000), "")
		);
	}

	#[test]
	fn logstore_json_logs_name_their_level() {
		let cases = [
			(r#"{"level":"error","msg":"x"}"#, Some(LogLevel::Error)),
			(r#"{"level":"WARNING","msg":"x"}"#, Some(LogLevel::Warn)),
			(r#"{"severity":"DEBUG"}"#, Some(LogLevel::Debug)),
			(r#"{"level":50,"msg":"pino"}"#, Some(LogLevel::Error)),
			(r#"{"level":30}"#, Some(LogLevel::Info)),
			(r#"{"msg":"0 errors"}"#, None),
			(r#"{"level":"loud"}"#, None),
		];
		for (line, want) in cases {
			assert_eq!(level_of(line), want, "{line}");
		}
	}

	#[test]
	fn logstore_text_logs_name_their_level_in_a_marker() {
		let cases = [
			("level=warn msg=\"slow\" took=3s", Some(LogLevel::Warn)),
			("ts=1 lvl=error msg=x", Some(LogLevel::Error)),
			("[ERROR] could not bind", Some(LogLevel::Error)),
			(
				"2026-11-21T08:00:00Z ERROR could not bind",
				Some(LogLevel::Error),
			),
			(
				"2026/11/21 08:00:00 [warn] upstream slow",
				Some(LogLevel::Warn),
			),
			("INFO:     127.0.0.1 - GET / 200", Some(LogLevel::Info)),
			("ERROR:root:boom", Some(LogLevel::Error)),
			("E1121 08:00:00.1 main.go:1] boom", Some(LogLevel::Error)),
			("DEBUG starting", Some(LogLevel::Debug)),
			("error: expected a value", Some(LogLevel::Error)),
			("an error occurred while starting", None),
			("Connection error", None),
			("Starting server on :8080", None),
			("", None),
		];
		for (line, want) in cases {
			assert_eq!(level_of(line), want, "{line}");
		}
	}
}
