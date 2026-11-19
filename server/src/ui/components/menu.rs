//! Dropdown menu: a popover opened by an invoker button (`command="toggle-popover"
//! commandfor=<id>`). The browser opens and closes it, dismisses it on Esc and on a click
//! outside, and anchors it to the button (CSS anchor positioning with flip fallbacks). The
//! items are a `focusgroup="menu nomemory"`: the browser moves focus with the arrow keys and
//! Home/End and infers the `menu` and `menuitem` roles. Where it cannot, `page.js` does all
//! of that, places the menu by hand when anchor positioning is missing, keeps `aria-expanded`
//! and closes the menu after a command.
//!
//! Items are links (`menu_link`) or commands. A command carries Datastar attributes, so
//! the page writes it with the class constants:
//! `button type="button" class=(MENU_ITEM) "data-on:click"=... { "Redeploy" }`.
//! A menu is for actions on one thing; navigation between sections is `tabs` or the
//! sidebar.

use maud::{Markup, html};

pub const MENU_ITEM: &str = "ui-menu-item";
pub const MENU_ITEM_DANGER: &str = "ui-menu-item ui-menu-item-danger";

/// A trigger button and its menu. `trigger_class` is a button class list (usually
/// `button_class(Variant::Outline, Size::Icon)`), `trigger` its content, and `label` the
/// accessible name of the menu ("Actions for api"). `id` must be unique on the page.
pub fn dropdown_menu(
	id: &str,
	trigger_class: &str,
	label: &str,
	trigger: &Markup,
	items: &Markup,
) -> Markup {
	let trigger_id = format!("{id}-trigger");
	html! {
		button id=(trigger_id) type="button" class={ (trigger_class) " ui-menu-trigger" }
			aria-haspopup="menu" aria-expanded="false" aria-label=(label)
			command="toggle-popover" commandfor=(id) { (trigger) }
		div id=(id) popover class="ui-menu" {
			div data-menu focusgroup="menu nomemory" aria-labelledby=(trigger_id) { (items) }
		}
	}
}

/// A menu item that navigates.
pub fn menu_link(href: &str, label: &Markup) -> Markup {
	html! { a href=(href) class=(MENU_ITEM) { (label) } }
}

/// A non-interactive heading for a group of items.
pub fn menu_label(text: &str) -> Markup {
	html! { div class="ui-menu-label" { (text) } }
}

pub fn menu_separator() -> Markup {
	html! { hr; }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> String {
		dropdown_menu(
			"row-1",
			"b",
			"Actions for <api>",
			&html! { "More" },
			&html! {
				(menu_label("App"))
				(menu_link("/a", &html! { "Open" }))
				(menu_separator())
				button type="button" class=(MENU_ITEM_DANGER) { "Delete" }
			},
		)
		.into_string()
	}

	#[test]
	fn the_trigger_is_an_invoker_for_the_popover() {
		let html = sample();
		assert!(html.contains("command=\"toggle-popover\""), "{html}");
		assert!(html.contains("commandfor=\"row-1\""), "{html}");
		assert!(html.contains("aria-haspopup=\"menu\""), "{html}");
		assert!(html.contains("aria-expanded=\"false\""), "{html}");
		assert!(!html.contains("popovertarget"), "{html}");
	}

	#[test]
	fn the_popover_follows_its_trigger_and_is_named_by_it() {
		let html = sample();
		let trigger_end = html.find("</button>").unwrap();
		let popover = html.find("<div id=\"row-1\" popover").unwrap();
		assert!(popover > trigger_end, "{html}");
		assert!(html.contains("aria-labelledby=\"row-1-trigger\""), "{html}");
		assert!(html.contains("data-menu"), "{html}");
	}

	#[test]
	fn the_label_is_escaped_and_a_separator_is_a_real_hr() {
		let html = sample();
		assert!(
			html.contains("aria-label=\"Actions for &lt;api&gt;\""),
			"{html}"
		);
		assert!(html.contains("<hr>"), "{html}");
		assert!(html.contains("ui-menu-item ui-menu-item-danger"), "{html}");
	}
}
