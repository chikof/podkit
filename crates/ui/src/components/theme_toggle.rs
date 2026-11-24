//! Theme toggle: system, light and dark as a three-button group. A form posting to
//! `/theme` (see `ui::theme`), so it needs no script; the pressed button shows the
//! current choice. Icon-only buttons are named by `aria-label`.

use maud::{Markup, html};

use super::icon::icon;
use crate::theme::{Theme, current};

const BUTTON: &str = "ui-seg-btn";

pub fn theme_toggle() -> Markup {
	toggle_for(current())
}

fn toggle_for(active: Theme) -> Markup {
	let options = [
		(Theme::System, "monitor", "System theme"),
		(Theme::Light, "sun", "Light theme"),
		(Theme::Dark, "moon", "Dark theme"),
	];
	html! {
		form method="post" action="/theme" role="group" aria-label="Color theme" class="ui-seg" {
			@for (theme, icon_name, label) in options {
				button type="submit" name="theme" value=(theme.as_str()) title=(label) aria-label=(label)
					aria-pressed=(if theme == active { "true" } else { "false" })
					class=(BUTTON) {
					(icon(icon_name))
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exactly_the_active_choice_is_pressed() {
		for active in [Theme::System, Theme::Light, Theme::Dark] {
			let html = toggle_for(active).into_string();
			assert_eq!(html.matches("aria-pressed=\"true\"").count(), 1, "{html}");
			assert_eq!(html.matches("aria-pressed=\"false\"").count(), 2, "{html}");
			let button = html
				.split("<button ")
				.find(|b| b.contains(&format!("value=\"{}\"", active.as_str())))
				.unwrap();
			assert!(button.contains("aria-pressed=\"true\""), "{html}");
		}
	}

	#[test]
	fn it_is_a_named_post_form_with_named_buttons() {
		let html = toggle_for(Theme::System).into_string();
		assert!(html.contains("method=\"post\" action=\"/theme\""), "{html}");
		assert!(
			html.contains("role=\"group\" aria-label=\"Color theme\""),
			"{html}"
		);
		for label in ["System theme", "Light theme", "Dark theme"] {
			assert!(html.contains(&format!("aria-label=\"{label}\"")), "{html}");
		}
	}
}
