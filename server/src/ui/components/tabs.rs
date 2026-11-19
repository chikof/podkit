//! Tabs. Two kinds, by what a tab does:
//!
//! - `tabs`: links, one per page of a section, server-rendered. The current one has
//!   `aria-current="page"`. These are navigation, not an ARIA tablist: each click is a page
//!   load, so they are plain links with no roving focus to maintain.
//! - `radio_tabs`: panels on one page, switched by native radio inputs. The panel follows the
//!   checked radio through `:has()` (`components.css`), so it needs no script; Tab lands on the
//!   selected tab and the arrow keys move and select, as the platform does for radios.

use maud::{Markup, html};

/// A tab link of a page, and the one for the page being shown.
pub const TAB: &str = "ui-tab";
pub const TAB_ACTIVE: &str = "ui-tab ui-tab-on";

pub struct TabItem<'a> {
	pub label: &'a str,
	pub href: &'a str,
	pub current: bool,
}

/// `label` names the navigation landmark ("Application sections"). On a narrow screen the
/// row scrolls sideways instead of wrapping.
pub fn tabs(label: &str, items: &[TabItem<'_>]) -> Markup {
	html! {
		nav aria-label=(label) class="ui-tabs" {
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

/// One tab of `radio_tabs`: the radio's `value`, the label and an optional count.
pub struct RadioTab<'a> {
	pub value: &'a str,
	pub label: &'a str,
	pub count: Option<&'a str>,
}

/// The most panels `components.css` knows how to switch.
pub const MAX_RADIO_TABS: usize = 8;

/// Panels on one page. `name` is the radio group (unique on the page) and `label` its
/// accessible name; `selected` is the index shown first; `panels` has one entry per tab.
/// Both lists are cut at `MAX_RADIO_TABS`.
pub fn radio_tabs(
	name: &str,
	label: &str,
	items: &[RadioTab<'_>],
	selected: usize,
	panels: &[Markup],
) -> Markup {
	let n = items.len().min(panels.len()).min(MAX_RADIO_TABS);
	html! {
		div class="ui-tabset" {
			div class="ui-tabs" role="radiogroup" aria-label=(label) {
				@for (i, item) in items.iter().take(n).enumerate() {
					input type="radio" name=(name) id={ (name) "-" (i) } value=(item.value) checked[i == selected];
					label for={ (name) "-" (i) } class="ui-tab" {
						(item.label)
						@if let Some(count) = item.count { span class="ui-tab-n" { (count) } }
					}
				}
			}
			@for panel in panels.iter().take(n) {
				div class="ui-panel" { (panel) }
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

	fn items() -> [RadioTab<'static>; 3] {
		[
			RadioTab {
				value: "production",
				label: "production",
				count: Some("6"),
			},
			RadioTab {
				value: "staging",
				label: "staging",
				count: None,
			},
			RadioTab {
				value: "<x>",
				label: "<b>x</b>",
				count: None,
			},
		]
	}

	#[test]
	fn radio_tabs_are_a_named_radio_group_with_one_checked_and_one_panel_each() {
		let html = radio_tabs(
			"env",
			"Environments",
			&items(),
			1,
			&[html! { "a" }, html! { "b" }, html! { "c" }],
		)
		.into_string();
		assert!(html.contains("role=\"radiogroup\""), "{html}");
		assert!(html.contains("aria-label=\"Environments\""), "{html}");
		assert_eq!(html.matches("type=\"radio\"").count(), 3, "{html}");
		assert_eq!(html.matches("checked").count(), 1, "{html}");
		assert_eq!(html.matches("class=\"ui-panel\"").count(), 3, "{html}");
		// A label follows its input, which is what the stylesheet's `input:checked + label` needs.
		assert!(
			html.contains("id=\"env-0\" value=\"production\"><label for=\"env-0\""),
			"{html}"
		);
		assert!(html.contains("<span class=\"ui-tab-n\">6</span>"), "{html}");
	}

	#[test]
	fn radio_tabs_escape_and_are_capped() {
		let html = radio_tabs("t", "T", &items(), 0, &[html! {}, html! {}, html! {}]).into_string();
		assert!(!html.contains("<b>"), "{html}");
		assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"), "{html}");
		let many: Vec<RadioTab<'_>> = (0..12)
			.map(|_| RadioTab {
				value: "v",
				label: "l",
				count: None,
			})
			.collect();
		let panels: Vec<Markup> = (0..12).map(|_| html! {}).collect();
		let html = radio_tabs("m", "M", &many, 0, &panels).into_string();
		assert_eq!(
			html.matches("type=\"radio\"").count(),
			MAX_RADIO_TABS,
			"{html}"
		);
		assert_eq!(
			html.matches("class=\"ui-panel\"").count(),
			MAX_RADIO_TABS,
			"{html}"
		);
	}
}
