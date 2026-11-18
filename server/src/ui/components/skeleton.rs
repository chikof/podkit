//! Skeleton: a placeholder with the shape of content that is loading. It is decorative
//! (`aria-hidden`); the region it stands in for says it is busy with `aria-busy="true"`.
//! Match the size of what will replace it so the page does not jump.

use maud::{Markup, html};

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
	fn the_class_is_escaped() {
		let html = skeleton("\"><script>").into_string();
		assert!(!html.contains("<script>"), "{html}");
	}
}
