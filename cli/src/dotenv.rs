//! `.env` files: reading them the way the server does, writing them back, and the
//! difference between a file and what an application has.
//!
//! The parser follows the server's (`env_vars::service::parse_dotenv`): blank lines and
//! `#` comments are skipped, `export ` is allowed, a value in matching quotes is taken as
//! it is, an unquoted one loses surrounding spaces and a trailing ` # comment`. A push
//! sends [`render`]ed text, so what the diff shows is exactly what the server stores.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// One variable of a file: its line, name and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
	/// 1-based.
	pub line: usize,
	/// The variable's name.
	pub key: String,
	/// Its value, unquoted.
	pub value: String,
}

/// A line that did not parse. It says where and why, never what the line held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
	/// 1-based.
	pub line: usize,
	/// What is wrong with it.
	pub message: &'static str,
}

/// Whether `key` is a name the server accepts.
#[must_use]
pub fn valid_key(key: &str) -> bool {
	let b = key.as_bytes();
	(1..=255).contains(&b.len())
		&& !b[0].is_ascii_digit()
		&& b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

fn unquote(value: &str) -> Option<&str> {
	let b = value.as_bytes();
	if b.len() >= 2 && matches!(b[0], b'"' | b'\'') && b[b.len() - 1] == b[0] {
		Some(&value[1..value.len() - 1])
	} else {
		None
	}
}

/// Parses `.env` text into its variables, in file order, and the lines it skipped.
#[must_use]
pub fn parse(text: &str) -> (Vec<Entry>, Vec<LineError>) {
	let (mut entries, mut errors) = (Vec::new(), Vec::new());
	for (i, raw) in text.lines().enumerate() {
		let line = i + 1;
		let trimmed = raw.trim();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let trimmed = trimmed
			.strip_prefix("export ")
			.map_or(trimmed, str::trim_start);
		let Some((key, value)) = trimmed.split_once('=') else {
			errors.push(LineError {
				line,
				message: "expected KEY=VALUE",
			});
			continue;
		};
		let key = key.trim();
		if !valid_key(key) {
			errors.push(LineError {
				line,
				message: "the name may only use letters, digits and underscores, and not start with a digit",
			});
			continue;
		}
		let value = value.trim();
		let value = match unquote(value) {
			Some(inner) => inner,
			None => value.split(" #").next().unwrap_or(value).trim_end(),
		};
		entries.push(Entry {
			line,
			key: key.to_owned(),
			value: value.to_owned(),
		});
	}
	(entries, errors)
}

/// The variables of a file by name; a name given twice keeps its last value, as the server
/// would after setting both in order.
#[must_use]
pub fn by_key(entries: &[Entry]) -> BTreeMap<String, String> {
	entries
		.iter()
		.map(|e| (e.key.clone(), e.value.clone()))
		.collect()
}

/// Writes variables as `.env` text, one `KEY="value"` per line after the `header` comment
/// lines. Double quotes are always used: the server takes a quoted value as it is, so any
/// value without a line break comes back unchanged. `None` writes `KEY=` (a name whose
/// value podkit does not hand out).
///
/// # Errors
/// When a value holds a line break, which a `.env` line cannot carry.
pub fn render<'a>(
	header: &[&str],
	vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Result<String, String> {
	let mut out = String::new();
	for h in header {
		let _ = writeln!(out, "# {h}");
	}
	for (key, value) in vars {
		match value {
			Some(v) if v.contains(['\n', '\r']) => {
				return Err(format!(
					"the value of {key} spans lines, which .env cannot hold"
				));
			}
			Some(v) => {
				let _ = writeln!(out, "{key}=\"{v}\"");
			}
			None => {
				let _ = writeln!(out, "{key}=");
			}
		}
	}
	Ok(out)
}

/// What a push would do to an application's variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
	/// In the file, not on the application: created.
	pub added: Vec<String>,
	/// In both: the value is replaced. Values are write-only, so whether it changes is
	/// not known.
	pub replaced: Vec<String>,
	/// On the application, not in the file: kept, or deleted with `--prune`.
	pub missing: Vec<String>,
	/// On the application, and empty in the file: left alone unless empty values are
	/// allowed. A pulled file has no values, and pushing it back must not blank them.
	pub blank: Vec<String>,
}

impl Diff {
	/// Compares the file's names with the application's. Unless `allow_empty`, a name the
	/// application has and the file leaves empty goes to [`blank`](Self::blank), not
	/// [`replaced`](Self::replaced).
	#[must_use]
	pub fn of(
		file: &BTreeMap<String, String>,
		remote: &BTreeSet<String>,
		allow_empty: bool,
	) -> Self {
		let mut d = Self::default();
		for (key, value) in file {
			if !remote.contains(key) {
				d.added.push(key.clone());
			} else if value.is_empty() && !allow_empty {
				d.blank.push(key.clone());
			} else {
				d.replaced.push(key.clone());
			}
		}
		d.missing = remote
			.iter()
			.filter(|k| !file.contains_key(*k))
			.cloned()
			.collect();
		d
	}

	/// The summary printed before a push asks to go ahead. Names only, never values.
	#[must_use]
	pub fn summary(&self, prune: bool) -> String {
		let mut out = String::new();
		for k in &self.added {
			let _ = writeln!(out, "  + {k}");
		}
		for k in &self.replaced {
			let _ = writeln!(out, "  ~ {k}");
		}
		for k in &self.blank {
			let _ = writeln!(
				out,
				"    {k} (empty in the file, kept; --allow-empty clears it)"
			);
		}
		for k in &self.missing {
			if prune {
				let _ = writeln!(out, "  - {k}");
			} else {
				let _ = writeln!(out, "    {k} (not in the file, kept)");
			}
		}
		let gone = if prune { self.missing.len() } else { 0 };
		let _ = writeln!(
			out,
			"{} to add, {} to set (values are write-only, so unchanged ones are set again), {gone} to delete",
			self.added.len(),
			self.replaced.len(),
		);
		out
	}

	/// The variables to send: the file's, less the [`blank`](Self::blank) ones.
	#[must_use]
	pub fn to_send<'a>(&self, file: &'a BTreeMap<String, String>) -> Vec<(&'a str, &'a str)> {
		file.iter()
			.filter(|(k, _)| !self.blank.contains(k))
			.map(|(k, v)| (k.as_str(), v.as_str()))
			.collect()
	}

	/// Whether the push changes nothing at all.
	#[must_use]
	pub fn is_empty(&self, prune: bool) -> bool {
		self.added.is_empty() && self.replaced.is_empty() && (!prune || self.missing.is_empty())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn triples(entries: &[Entry]) -> Vec<(usize, &str, &str)> {
		entries
			.iter()
			.map(|e| (e.line, e.key.as_str(), e.value.as_str()))
			.collect()
	}

	#[test]
	fn parses_like_the_server() {
		// The server's own test case.
		let text = "# comment\n\nA=1\nexport B = two words \nC=\"q # not comment\"\nD='x'\nE=v # trailing\nF=\n";
		let (entries, errors) = parse(text);
		assert!(errors.is_empty(), "{errors:?}");
		assert_eq!(
			triples(&entries),
			[
				(3, "A", "1"),
				(4, "B", "two words"),
				(5, "C", "q # not comment"),
				(6, "D", "x"),
				(7, "E", "v"),
				(8, "F", ""),
			]
		);
	}

	#[test]
	fn bad_lines_are_reported_by_number_without_their_content() {
		let (entries, errors) = parse("OK=1\nnot a pair S3CR3T\n1BAD=2\n=3\nGOOD=4\r\n");
		assert_eq!(triples(&entries), [(1, "OK", "1"), (5, "GOOD", "4")]);
		assert_eq!(errors.iter().map(|e| e.line).collect::<Vec<_>>(), [2, 3, 4]);
		assert!(errors.iter().all(|e| !e.message.contains("S3CR3T")));
	}

	#[test]
	fn a_key_given_twice_keeps_the_last_value() {
		let (entries, _) = parse("A=1\nB=2\nA=3");
		let map = by_key(&entries);
		assert_eq!(map.get("A").map(String::as_str), Some("3"));
		assert_eq!(map.len(), 2);
	}

	#[test]
	fn render_round_trips_awkward_values() {
		let values = [
			("PLAIN", "abc"),
			("SPACES", "  padded  "),
			("HASH", "a # b"),
			("QUOTES", r#"say "hi" it's"#),
			("EMPTY", ""),
			("EQUALS", "a=b=c"),
			("TRAILING_QUOTE", "ends with \""),
		];
		let text = render(&["pulled"], values.iter().map(|(k, v)| (*k, Some(*v)))).unwrap();
		assert!(text.starts_with("# pulled\n"));
		let (entries, errors) = parse(&text);
		assert!(errors.is_empty(), "{errors:?}");
		let got: Vec<(&str, &str)> = entries
			.iter()
			.map(|e| (e.key.as_str(), e.value.as_str()))
			.collect();
		assert_eq!(got, values);
	}

	#[test]
	fn render_writes_names_without_values_and_refuses_line_breaks() {
		let text = render(&[], [("A", None), ("B", Some("1"))]).unwrap();
		assert_eq!(text, "A=\nB=\"1\"\n");
		assert!(render(&[], [("A", Some("x\ny"))]).is_err());
	}

	#[test]
	fn diff_sorts_names_into_added_replaced_and_missing() {
		let (entries, _) = parse("NEW=1\nKEEP=2\nB_NEW=3");
		let remote: BTreeSet<String> = ["KEEP", "OLD"].iter().map(|s| (*s).to_owned()).collect();
		let d = Diff::of(&by_key(&entries), &remote, false);
		assert_eq!(d.added, ["B_NEW", "NEW"]);
		assert_eq!(d.replaced, ["KEEP"]);
		assert_eq!(d.missing, ["OLD"]);
		assert!(!d.is_empty(false));

		let kept = d.summary(false);
		assert!(kept.contains("  + NEW") && kept.contains("  ~ KEEP"));
		assert!(kept.contains("OLD (not in the file, kept)"));
		assert!(kept.contains("2 to add, 1 to set"));
		assert!(kept.contains("0 to delete"));
		let pruned = d.summary(true);
		assert!(pruned.contains("  - OLD") && pruned.contains("1 to delete"));

		let none = Diff::of(&BTreeMap::new(), &remote, false);
		assert!(none.is_empty(false));
		assert!(!none.is_empty(true));
	}

	#[test]
	fn the_summary_never_holds_a_value() {
		let (entries, _) = parse("TOKEN=hunter2");
		let d = Diff::of(&by_key(&entries), &BTreeSet::new(), false);
		assert!(!d.summary(true).contains("hunter2"));
	}

	#[test]
	fn a_pulled_file_pushed_back_blanks_nothing() {
		// What `env pull` writes when values are not handed out.
		let pulled = render(&["pulled"], [("A", None), ("B", None)]).unwrap();
		let (entries, _) = parse(&format!("{pulled}C=new\nB=set\n"));
		let file = by_key(&entries);
		let remote: BTreeSet<String> = ["A", "B"].iter().map(|s| (*s).to_owned()).collect();
		let d = Diff::of(&file, &remote, false);
		assert_eq!(d.blank, ["A"]);
		assert_eq!(d.replaced, ["B"]);
		assert_eq!(d.added, ["C"]);
		assert_eq!(d.to_send(&file), [("B", "set"), ("C", "new")]);
		assert!(d.summary(false).contains("A (empty in the file, kept"));
		// Asked for, an empty value is sent like any other.
		let d = Diff::of(&file, &remote, true);
		assert!(d.blank.is_empty());
		assert_eq!(d.to_send(&file).len(), 3);
	}
}
