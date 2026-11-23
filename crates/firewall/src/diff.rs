//! A line diff for the preview: what applying would add and remove.

use serde::{Deserialize, Serialize};

/// One line of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
	/// `' '` kept, `'+'` added, `'-'` removed.
	pub op: char,
	/// The line without its newline.
	pub text: String,
}

/// Lines of `new` against `old`, longest-common-subsequence based. Rulesets are a few
/// hundred lines, so the quadratic table is fine.
#[must_use]
pub fn diff(old: &str, new: &str) -> Vec<DiffLine> {
	let a: Vec<&str> = old.lines().collect();
	let b: Vec<&str> = new.lines().collect();
	let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
	for i in (0..a.len()).rev() {
		for j in (0..b.len()).rev() {
			lcs[i][j] = if a[i] == b[j] {
				lcs[i + 1][j + 1] + 1
			} else {
				lcs[i + 1][j].max(lcs[i][j + 1])
			};
		}
	}
	let (mut i, mut j) = (0, 0);
	let mut out = Vec::new();
	let mut line = |op, text: &str| {
		out.push(DiffLine {
			op,
			text: text.to_owned(),
		});
	};
	while i < a.len() && j < b.len() {
		if a[i] == b[j] {
			line(' ', a[i]);
			i += 1;
			j += 1;
		} else if lcs[i + 1][j] >= lcs[i][j + 1] {
			line('-', a[i]);
			i += 1;
		} else {
			line('+', b[j]);
			j += 1;
		}
	}
	for l in &a[i..] {
		line('-', l);
	}
	for l in &b[j..] {
		line('+', l);
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn marks_added_and_removed_lines() {
		let d = diff("a\nb\nc\n", "a\nc\nd\n");
		let ops: String = d.iter().map(|l| l.op).collect();
		assert_eq!(ops, " - +");
	}

	#[test]
	fn identical_text_has_no_changes() {
		assert!(diff("x\ny\n", "x\ny\n").iter().all(|l| l.op == ' '));
	}
}
