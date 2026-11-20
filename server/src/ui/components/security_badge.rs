//! Security badge: what scanning found, as one small pill (`Clean`, `Low`, `Medium`,
//! `High`, `Critical`) and, when something needs attention first, an `Act now` marker
//! beside it. A badge never relies on color alone: the word is always there, and each
//! level has its own mark (diamond, disc, triangle, ring, dash, check) drawn by
//! `components.css` from `data-level`.
//!
//! A resource that was never scanned has no badge at all, so a list of resources is not
//! littered with "not scanned" pills.

use maud::{Markup, html};

use crate::scanning::{Badge, BadgeState, Severity};

/// The value of `data-level`, which the stylesheet keys color and mark on.
const fn level(state: BadgeState) -> Option<&'static str> {
	match state {
		BadgeState::None => None,
		BadgeState::Scanning => Some("scanning"),
		BadgeState::Failed => Some("failed"),
		BadgeState::Clean => Some("clean"),
		BadgeState::Found(Severity::Critical) => Some("critical"),
		BadgeState::Found(Severity::High) => Some("high"),
		BadgeState::Found(Severity::Medium) => Some("medium"),
		BadgeState::Found(Severity::Low) => Some("low"),
		BadgeState::Found(Severity::Unknown) => Some("unknown"),
	}
}

const fn word(state: BadgeState) -> &'static str {
	match state {
		BadgeState::None => "",
		BadgeState::Scanning => "Scanning",
		BadgeState::Failed => "Scan failed",
		BadgeState::Clean => "Clean",
		BadgeState::Found(severity) => severity.label(),
	}
}

/// The pill for one severity, as the findings table shows it.
#[must_use]
pub fn severity_chip(severity: Severity) -> Markup {
	html! {
		span class="ui-sev" data-level=(severity.as_str()) {
			span class="ui-sev-mark" aria-hidden="true" {}
			span { (severity.label()) }
		}
	}
}

/// The "Act now" marker. `reason` says why, for a pointer and for assistive technology.
#[must_use]
pub fn act_now_marker(reason: &str) -> Markup {
	html! {
		span class="ui-sev-act" title=(reason) { "Act now" }
	}
}

/// A resource's badge. Empty for something never scanned. The counts are the tooltip
/// ("2 high, 1 medium (1 act now)"), and the whole badge may link to the Security tab.
#[must_use]
pub fn security_badge(badge: &Badge, href: Option<&str>) -> Markup {
	let Some(level) = level(badge.state) else {
		return html! {};
	};
	let found = matches!(badge.state, BadgeState::Found(_) | BadgeState::Clean);
	let summary = if found {
		badge.counts.summary()
	} else {
		word(badge.state).to_owned()
	};
	let inner = html! {
		span class="ui-sev" data-level=(level) {
			span class="ui-sev-mark" aria-hidden="true" {}
			span { (word(badge.state)) }
		}
		@if badge.counts.act_now > 0 {
			(act_now_marker("Listed as actively exploited, or likely to be and severe"))
		}
	};
	html! {
		@if let Some(href) = href {
			a class="ui-sev-group" href=(href) title=(summary) aria-label={ "Security: " (summary) } { (inner) }
		} @else {
			span class="ui-sev-group" title=(summary) { (inner) }
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scanning::Counts;

	fn found(severity: Severity, n: i64, act_now: i64) -> Badge {
		let mut counts = Counts::default();
		for i in 0..n {
			counts.add(severity, i < act_now);
		}
		Badge::found(counts)
	}

	#[test]
	fn a_badge_says_the_worst_level_in_words_and_never_by_color_alone() {
		let html = security_badge(&found(Severity::High, 2, 0), None).into_string();
		assert!(html.contains("data-level=\"high\""), "{html}");
		assert!(html.contains("<span>High</span>"), "{html}");
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(html.contains("title=\"2 high\""), "{html}");
		assert!(!html.contains("Act now"), "{html}");
	}

	#[test]
	fn act_now_is_a_marker_next_to_the_level() {
		let html = security_badge(&found(Severity::Critical, 3, 1), None).into_string();
		assert!(html.contains("data-level=\"critical\""), "{html}");
		assert!(html.contains("Act now"), "{html}");
		assert!(html.contains("3 critical (1 act now)"), "{html}");
	}

	#[test]
	fn every_state_but_never_scanned_has_a_word() {
		assert_eq!(
			security_badge(&Badge::NONE, None).into_string(),
			"",
			"never scanned: no badge"
		);
		for (state, text) in [
			(BadgeState::Scanning, "Scanning"),
			(BadgeState::Failed, "Scan failed"),
			(BadgeState::Clean, "Clean"),
			(BadgeState::Found(Severity::Low), "Low"),
			(BadgeState::Found(Severity::Medium), "Medium"),
			(BadgeState::Found(Severity::Unknown), "Unknown"),
		] {
			let html = security_badge(
				&Badge {
					state,
					counts: Counts::default(),
				},
				None,
			)
			.into_string();
			assert!(html.contains(&format!("<span>{text}</span>")), "{html}");
		}
	}

	#[test]
	fn a_linked_badge_is_one_named_link() {
		let html =
			security_badge(&found(Severity::High, 1, 0), Some("/teams/1/security")).into_string();
		assert!(
			html.starts_with("<a class=\"ui-sev-group\" href=\"/teams/1/security\""),
			"{html}"
		);
		assert!(html.contains("aria-label=\"Security: 1 high\""), "{html}");
	}
}
