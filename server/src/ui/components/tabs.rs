//! Tabs: links, one per page of a section, server-rendered. The current one has
//! `aria-current="page"`. Tabs here are navigation, not an ARIA tablist: each click is a
//! page load, so they are plain links with no roving focus to maintain.

use maud::{Markup, html};

/// A tab link of a page, and the one for the page being shown.
pub const TAB: &str = "-mb-px border-b-2 border-transparent px-3 py-2 text-sm font-medium whitespace-nowrap text-muted-foreground transition-colors hover:text-foreground";
pub const TAB_ACTIVE: &str = "-mb-px border-b-2 border-foreground px-3 py-2 text-sm font-medium whitespace-nowrap text-foreground";

pub struct TabItem<'a> {
	pub label: &'a str,
	pub href: &'a str,
	pub current: bool,
}

/// `label` names the navigation landmark ("Application sections"). On a narrow screen the
/// row scrolls sideways instead of wrapping.
pub fn tabs(label: &str, items: &[TabItem<'_>]) -> Markup {
	html! {
		nav aria-label=(label) class="flex gap-1 overflow-x-auto overflow-y-hidden border-b border-border" {
			@for item in items {
				@if item.current {
					a href=(item.href) class=(TAB_ACTIVE) aria-current="page" { (item.label) }
				} @else {
					a href=(item.href) class=(TAB) { (item.label) }
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_the_current_tab_is_marked() {
		let html = tabs(
			"Sections",
			&[
				TabItem {
					label: "Overview",
					href: "/a",
					current: false,
				},
				TabItem {
					label: "Logs",
					href: "/b",
					current: true,
				},
			],
		)
		.into_string();
		assert_eq!(html.matches("aria-current=\"page\"").count(), 1, "{html}");
		assert!(html.contains("aria-label=\"Sections\""), "{html}");
		let (_, after) = html.split_once("aria-current=\"page\"").unwrap();
		assert!(after.starts_with(">Logs"), "{html}");
	}

	#[test]
	fn labels_and_hrefs_are_escaped() {
		let html = tabs(
			"x",
			&[TabItem {
				label: "<i>a</i>",
				href: "/\"y",
				current: false,
			}],
		)
		.into_string();
		assert!(!html.contains("<i>"), "{html}");
		assert!(html.contains("href=\"/&quot;y\""), "{html}");
	}
}
