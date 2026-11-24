//! `icon(name)`: a Lucide icon from the vendored sprite (`/static/icons.svg`). Decorative
//! by default (`aria-hidden`): the control it sits in carries the accessible name, so an
//! icon-only button needs an `aria-label` (see `icon_button`).
//!
//! It is as big as the text around it (`1em`) and takes the text color. Add a size
//! with `icon_with(name, "h-5 w-5")`. Names are those in `scripts/vendor-ui-assets.py`.

use maud::{Markup, html};

use crate::assets::ICONS_HREF;

/// An icon sized to the surrounding text.
pub fn icon(name: &str) -> Markup {
	icon_with(name, "")
}

/// An icon with extra utility classes, e.g. `"h-5 w-5 text-muted-foreground"`.
pub fn icon_with(name: &str, class: &str) -> Markup {
	html! {
		svg class={ "ui-icon " (class) } aria-hidden="true" focusable="false" data-icon=(name) {
			use href={ (*ICONS_HREF) "#" (name) } {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::assets::ICONS_SVG;

	#[test]
	fn an_icon_is_decorative_and_points_into_the_hashed_sprite() {
		let html = icon("check").into_string();
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(html.contains("/static/icons.svg?v="), "{html}");
		assert!(html.contains("#check\""), "{html}");
	}

	#[test]
	fn the_name_and_class_are_escaped() {
		let html = icon_with("x\"><script>", "a\"b").into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("&quot;"), "{html}");
	}

	#[test]
	fn every_icon_the_design_system_uses_is_in_the_sprite() {
		for name in crate::gallery::ICON_NAMES {
			assert!(
				ICONS_SVG.contains(&format!("<symbol id=\"{name}\"")),
				"missing icon {name}"
			);
		}
	}
}
