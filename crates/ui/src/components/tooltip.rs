//! Tooltip: a short hint on the control itself, shown on hover and keyboard focus. It is a
//! `popover="hint"` opened by `interestfor` on its trigger, so the browser handles the
//! timing, Esc (WCAG 1.4.13) and light dismiss; the trigger is the popover's implicit
//! anchor, so CSS anchor positioning puts it below, flipping above when there is no room.
//!
//! Where `interestfor` or anchor positioning is missing `page.js` shows and places the same
//! popover on hover and focus; without script the control still works and keeps its name.
//!
//! Use it to name an icon-only control or to add a clause to a label. Anything the user
//! needs to act belongs in the page, not in a tooltip; it is unreachable on touch.
//!
//! Accessibility is wired here, not left to the browser: an icon-only trigger gets the tip as
//! its `aria-label`; a trigger that has its own name gets the tip as `aria-describedby`,
//! unless the tip only repeats that name.

use maud::{Markup, html};

/// A button `trigger` (its content, e.g. an icon or a label) with the hint `text`. `id` must
/// be unique on the page; `class` is a button class list; `label` is the trigger's own
/// accessible name when it has no visible text (`None` when the content is a visible label,
/// in which case the tip describes it).
pub fn tooltip(id: &str, text: &str, class: &str, label: Option<&str>, trigger: &Markup) -> Markup {
	let name = label.unwrap_or(text);
	let described = label.is_some_and(|l| l != text) || label.is_none() && !trigger.0.is_empty();
	html! {
		button type="button" class=(class) interestfor=(id) aria-label=[label.map(|_| name)]
			aria-describedby=[described.then_some(id)] { (trigger) }
		div id=(id) popover="hint" class="ui-tip" { (text) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_tip_is_a_hint_popover_opened_by_interestfor() {
		let html = tooltip(
			"tip-1",
			"Redeploy <now>",
			"c",
			Some("Redeploy"),
			&html! { "R" },
		)
		.into_string();
		assert!(html.contains("interestfor=\"tip-1\""), "{html}");
		assert!(
			html.contains("<div id=\"tip-1\" popover=\"hint\" class=\"ui-tip\">"),
			"{html}"
		);
		assert!(html.contains("Redeploy &lt;now&gt;"), "{html}");
		assert!(!html.contains("<now>"), "{html}");
		// The trigger has its own name and the tip says more: it describes it.
		assert!(html.contains("aria-label=\"Redeploy\""), "{html}");
		assert!(html.contains("aria-describedby=\"tip-1\""), "{html}");
	}

	#[test]
	fn a_tip_that_repeats_the_name_is_not_read_twice() {
		let html = tooltip("t", "Copy", "c", Some("Copy"), &html! { "x" }).into_string();
		assert!(html.contains("aria-label=\"Copy\""), "{html}");
		assert!(!html.contains("aria-describedby"), "{html}");
	}

	#[test]
	fn a_labelled_trigger_is_described_by_the_tip() {
		let html =
			tooltip("t", "Needs the owner role", "c", None, &html! { "Delete" }).into_string();
		assert!(!html.contains("aria-label"), "{html}");
		assert!(html.contains("aria-describedby=\"t\""), "{html}");
	}
}
