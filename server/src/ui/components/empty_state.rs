//! Empty state: what a list shows when it has nothing, and what to do about it. One icon,
//! a headline that says what is missing, one sentence on why it matters or how to fix it,
//! and up to three actions (`action` holds them: a primary button first, then links).

use maud::{Markup, html};

use super::icon::icon_with;

pub fn empty_state(
	icon_name: &str,
	title: &str,
	description: Option<&str>,
	action: Option<&Markup>,
) -> Markup {
	html! {
		div class="ui-empty" {
			div class="ui-empty-ic" { (icon_with(icon_name, "")) }
			h2 class="ui-empty-title" { (title) }
			@if let Some(description) = description {
				p class="ui-empty-text" { (description) }
			}
			@if let Some(action) = action {
				div class="ui-empty-act" { (action) }
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
			html.contains("<h2 class=\"ui-empty-title\">No apps yet</h2>"),
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
