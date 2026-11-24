//! Filter chip: a link that narrows a list, such as a size class or a tag. A page filtered
//! by it is a plain GET, so it works without script and the result can be shared. The chip
//! that is on carries `aria-current="true"`; the page keeps its own "all" chip.

use maud::{Markup, html};

/// A group of chips under one name, so a screen reader hears "Size, group" once.
pub fn chip_group(label: &str, chips: &Markup) -> Markup {
	html! {
		div class="flex flex-wrap items-center gap-2" role="group" aria-label=(label) {
			span class="w-14 shrink-0 text-sm text-muted-foreground" aria-hidden="true" { (label) }
			(chips)
		}
	}
}

/// One chip. `hint` is a tooltip for what the label stands for ("under 8B").
pub fn chip(label: &str, href: &str, on: bool, hint: Option<&str>) -> Markup {
	html! {
		a class="ui-chip" href=(href) aria-current=[on.then_some("true")] title=[hint] { (label) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_the_chip_that_is_on_is_marked_current() {
		let on = chip("Tiny", "/x?size=tiny", true, Some("under 8B")).into_string();
		let off = chip("Small", "/x?size=small", false, None).into_string();
		assert!(on.contains("aria-current=\"true\""), "{on}");
		assert!(on.contains("title=\"under 8B\""), "{on}");
		assert!(!off.contains("aria-current"), "{off}");
		assert!(!off.contains("title="), "{off}");
	}

	#[test]
	fn labels_and_hrefs_are_escaped_and_a_group_is_named() {
		let html = chip_group("Size <x>", &chip("<b>a</b>", "/\"y", false, None)).into_string();
		assert!(!html.contains("<b>"), "{html}");
		assert!(html.contains("href=\"/&quot;y\""), "{html}");
		assert!(html.contains("role=\"group\""), "{html}");
		assert!(html.contains("aria-label=\"Size &lt;x&gt;\""), "{html}");
	}
}
