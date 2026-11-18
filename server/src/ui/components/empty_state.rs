//! Empty state: what a list shows when it has nothing, and what to do about it. One icon,
//! a headline that says what is missing, one sentence on why it matters or how to fix it,
//! and at most one action.

use maud::{Markup, html};

use super::icon::icon_with;

pub fn empty_state(
	icon_name: &str,
	title: &str,
	description: Option<&str>,
	action: Option<&Markup>,
) -> Markup {
	html! {
		div class="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border px-6 py-12 text-center" {
			div class="flex h-12 w-12 items-center justify-center rounded-full bg-muted text-muted-foreground" {
				(icon_with(icon_name, "h-6 w-6"))
			}
			div class="flex max-w-sm flex-col gap-1" {
				h2 class="text-base font-semibold" { (title) }
				@if let Some(description) = description {
					p class="text-sm text-muted-foreground" { (description) }
				}
			}
			@if let Some(action) = action {
				div class="mt-1" { (action) }
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_empty_state_has_a_heading_and_an_optional_action() {
		let html = empty_state(
			"rocket",
			"No apps yet",
			Some("Deploy one."),
			Some(&html! { a href="/new" { "Create" } }),
		)
		.into_string();
		assert!(
			html.contains("<h2 class=\"text-base font-semibold\">No apps yet</h2>"),
			"{html}"
		);
		assert!(html.contains("href=\"/new\""), "{html}");
		let bare = empty_state("rocket", "Nothing", None, None).into_string();
		assert!(!bare.contains("<a "), "{bare}");
	}

	#[test]
	fn text_is_escaped() {
		let html = empty_state("x", "<b>t</b>", Some("<i>d</i>"), None).into_string();
		assert!(!html.contains("<b>") && !html.contains("<i>"), "{html}");
	}
}
