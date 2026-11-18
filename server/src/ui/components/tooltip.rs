//! Tooltip: a short hint on the control itself, shown on hover and keyboard focus,
//! anchored to it with CSS anchor positioning (above, flipping below when there is no
//! room). Pure CSS: it needs no script to show, and it stays visible while the pointer is
//! over it. Esc hides it (`podkit.js`).
//!
//! Use it to name an icon-only control or to add a clause to a label. Anything the user
//! needs to act belongs in the page, not in a tooltip; it is unreachable on touch.
//! The trigger keeps its own accessible name; the tooltip is a description, so give the
//! trigger `aria-describedby=<id>`.

use maud::{Markup, html};

/// `trigger` is the focusable element (a button or link) the tip describes; `id` must be
/// unique on the page and goes in the trigger's `aria-describedby`.
pub fn tooltip(id: &str, text: &str, trigger: &Markup) -> Markup {
	html! {
		span class="ui-tooltip" {
			(trigger)
			span id=(id) role="tooltip" class="ui-tooltip-content" { (text) }
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_tip_has_the_tooltip_role_and_the_id_for_describedby() {
		let html = tooltip(
			"tip-1",
			"Redeploy <now>",
			&html! { button aria-describedby="tip-1" { "R" } },
		)
		.into_string();
		assert!(
			html.contains("<span id=\"tip-1\" role=\"tooltip\""),
			"{html}"
		);
		assert!(html.contains("aria-describedby=\"tip-1\""), "{html}");
		assert!(html.contains("Redeploy &lt;now&gt;"), "{html}");
		assert!(html.starts_with("<span class=\"ui-tooltip\""), "{html}");
	}
}
