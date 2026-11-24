//! Usage meter: one line of the usage page. The name, the figures as text, and a native
//! `<progress>` when there is a limit to measure against (a screen reader gets the role and
//! the value from the element; see `progress.rs`). The state is also written in words, so it
//! never depends on the bar's colour: "Almost at the limit" from 80%, "Limit reached" at 100%.
//!
//! Styled by the `usage and limits` block of `components.css`.

use maud::{Markup, html};

/// How full a limit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterLevel {
	/// Under the soft limit.
	Ok,
	/// From 80%.
	Warn,
	/// At the limit.
	Full,
}

impl MeterLevel {
	const fn key(self) -> &'static str {
		match self {
			Self::Ok => "ok",
			Self::Warn => "warn",
			Self::Full => "full",
		}
	}

	const fn words(self) -> Option<&'static str> {
		match self {
			Self::Ok => None,
			Self::Warn => Some("Almost at the limit"),
			Self::Full => Some("Limit reached"),
		}
	}
}

/// `name` is the quota ("Memory"), `used` what is in use ("1.5 GB"), `limit` what is allowed
/// ("4 GB"), `percent` 0 to 100. Without a `limit` the line is text only.
#[must_use]
pub fn usage_meter(
	name: &str,
	used: &str,
	limit: Option<&str>,
	percent: u64,
	level: MeterLevel,
) -> Markup {
	let percent = percent.min(100);
	html! {
		div class="ui-usage-row" {
			div class="ui-usage-head" {
				span class="ui-usage-name" { (name) }
				@if let Some(limit) = limit {
					span class="ui-usage-fig" { (used) " of " (limit) }
				} @else {
					span class="ui-usage-fig" { (used) " · no limit" }
				}
			}
			@if let Some(limit) = limit {
				progress class="ui-progress" max="100" value=(percent) data-level=(level.key())
					aria-label=(name) aria-valuetext={ (used) " of " (limit) } {}
				@if let Some(words) = level.words() {
					span class="ui-usage-note" data-level=(level.key()) { (words) }
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_limit_gets_a_labelled_bar_and_the_state_in_words() {
		let html =
			usage_meter("Memory", "3.5 GB", Some("4 GB"), 87, MeterLevel::Warn).into_string();
		assert!(html.contains("<progress class=\"ui-progress\""), "{html}");
		assert!(html.contains("value=\"87\""), "{html}");
		assert!(html.contains("aria-label=\"Memory\""), "{html}");
		assert!(html.contains("3.5 GB of 4 GB"), "{html}");
		assert!(html.contains("Almost at the limit"), "{html}");
	}

	#[test]
	fn no_limit_is_text_only() {
		let html = usage_meter("Disk", "12 GB", None, 0, MeterLevel::Ok).into_string();
		assert!(!html.contains("<progress"), "{html}");
		assert!(html.contains("12 GB · no limit"), "{html}");
	}

	#[test]
	fn a_full_limit_says_so_and_names_are_escaped() {
		let html = usage_meter("<b>x</b>", "5", Some("5"), 140, MeterLevel::Full).into_string();
		assert!(html.contains("value=\"100\""), "{html}");
		assert!(html.contains("Limit reached"), "{html}");
		assert!(!html.contains("<b>x"), "{html}");
	}
}
