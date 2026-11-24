//! Skeleton: a placeholder with the shape of content that is loading. It is decorative
//! (`aria-hidden`); the region it stands in for says it is busy with `aria-busy="true"`.
//! Match the size of what will replace it so the page does not jump.

use maud::{Markup, html};

/// One row of a list, with the geometry of the real row (a logo tile, two lines, a status), so
/// the page does not jump when the content arrives. Put a few in a region marked
/// `aria-busy="true"`.
pub fn skeleton_row() -> Markup {
	html! {
		div class="ui-skeleton-row" aria-hidden="true" {
			(skeleton("h-8 w-8"))
			div { (skeleton("h-3.5 w-2/5")) (skeleton("h-3 w-3/4")) }
			(skeleton("h-3.5 w-20"))
		}
	}
}

/// `class` gives it a size, e.g. `"h-4 w-40"` or `"h-10 w-10 rounded-full"`.
pub fn skeleton(class: &str) -> Markup {
	html! { span class={ "ui-skeleton " (class) } aria-hidden="true" {} }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_skeleton_is_hidden_from_assistive_technology() {
		let html = skeleton("h-4 w-40").into_string();
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(html.contains("ui-skeleton h-4 w-40"), "{html}");
	}

	#[test]
	fn a_skeleton_row_is_decorative_and_has_the_row_geometry() {
		let html = skeleton_row().into_string();
		assert!(html.contains("ui-skeleton-row"), "{html}");
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert_eq!(html.matches("ui-skeleton ").count(), 4, "{html}");
	}

	#[test]
	fn the_class_is_escaped() {
		let html = skeleton("\"><script>").into_string();
		assert!(!html.contains("<script>"), "{html}");
	}
}
