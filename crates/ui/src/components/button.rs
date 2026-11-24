//! Buttons. Pick the variant by what the action means, not by how loud it should look:
//! one `Default` per view, `Destructive` only where the action destroys something.
//!
//! A button that carries Datastar attributes (`data-on:click`, `data-indicator:...`)
//! is written in the page as `button class=(BUTTON) ...` or
//! `class=(button_class(Variant::Outline, Size::Sm))`; maud cannot forward arbitrary
//! attributes through a function. `button`, `icon_button` and `link_button` render
//! the plain cases.

use maud::{Markup, html};

use super::icon::icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
	/// The main action of a view: the accent button.
	Default,
	/// A supporting action next to the main one.
	Secondary,
	/// A bordered action; the usual choice for the second button. Looks like `Secondary`.
	Outline,
	/// A low-emphasis action, e.g. in a toolbar or a row.
	Ghost,
	/// An action that deletes or stops something, drawn solid.
	Destructive,
	/// Reads as a link but acts as a button.
	Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
	Sm,
	Md,
	Lg,
	/// Square, for an icon-only button.
	Icon,
}

// The skin of every variant and size is in `components.css` (`ui-button-*`).
const DEFAULT: &str = "ui-button ui-button-default";
const SECONDARY: &str = "ui-button ui-button-secondary";
const OUTLINE: &str = "ui-button ui-button-outline";
const GHOST: &str = "ui-button ui-button-ghost";
const DESTRUCTIVE: &str = "ui-button ui-button-danger-solid";
const LINK: &str = "ui-button ui-button-link";

const SM: &str = "ui-button-sm";
const MD: &str = "";
const LG: &str = "ui-button-lg";
const ICON: &str = "ui-button-icon";

/// The main action on a page.
pub const BUTTON: &str = "ui-button ui-button-default";

/// A secondary action, drawn as a bordered button.
pub const BUTTON_OUTLINE: &str = "ui-button ui-button-outline";

/// A destructive action drawn as a bordered button in the error color: quieter than
/// `Variant::Destructive`, for a delete next to other actions in a row or card.
pub const BUTTON_DANGER: &str = "ui-button ui-button-danger";

/// The class list of a button, for markup that adds its own attributes.
pub fn button_class(variant: Variant, size: Size) -> String {
	let variant = match variant {
		Variant::Default => DEFAULT,
		Variant::Secondary => SECONDARY,
		Variant::Outline => OUTLINE,
		Variant::Ghost => GHOST,
		Variant::Destructive => DESTRUCTIVE,
		Variant::Link => LINK,
	};
	let size = match size {
		Size::Sm => SM,
		Size::Md => MD,
		Size::Lg => LG,
		Size::Icon => ICON,
	};
	if size.is_empty() {
		variant.to_owned()
	} else {
		format!("{variant} {size}")
	}
}

/// A `<button>` with a text label. `kind` is the `type`: `"button"` or `"submit"`.
pub fn button(variant: Variant, size: Size, kind: &str, label: &Markup) -> Markup {
	let kind = if kind == "submit" { "submit" } else { "button" };
	html! {
		button type=(kind) class=(button_class(variant, size)) { (label) }
	}
}

/// An icon-only button. `label` is its accessible name and is required: an icon alone
/// says nothing to a screen reader.
pub fn icon_button(variant: Variant, label: &str, icon_name: &str) -> Markup {
	html! {
		button type="button" class=(button_class(variant, Size::Icon)) aria-label=(label) { (icon(icon_name)) }
	}
}

/// A link that looks like a button, for navigation (a button is for actions).
pub fn link_button(variant: Variant, size: Size, href: &str, label: &Markup) -> Markup {
	html! {
		a href=(href) class=(button_class(variant, size)) { (label) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_variant_and_size_has_its_own_classes() {
		let all = [
			Variant::Default,
			Variant::Secondary,
			Variant::Outline,
			Variant::Ghost,
			Variant::Destructive,
			Variant::Link,
		];
		let classes: Vec<String> = all.iter().map(|v| button_class(*v, Size::Md)).collect();
		for (i, a) in classes.iter().enumerate() {
			assert!(a.starts_with("ui-button "), "{a}");
			for b in &classes[i + 1..] {
				assert_ne!(a, b);
			}
		}
		assert!(button_class(Variant::Default, Size::Icon).contains("ui-button-icon"));
		assert!(button_class(Variant::Default, Size::Sm).contains("ui-button-sm"));
		assert!(button_class(Variant::Default, Size::Lg).contains("ui-button-lg"));
		assert!(button_class(Variant::Destructive, Size::Md).contains("ui-button-danger-solid"));
	}

	#[test]
	fn an_icon_button_is_named_and_its_icon_is_hidden() {
		let html = icon_button(Variant::Ghost, "Delete \"db\"", "trash-2").into_string();
		assert!(
			html.contains("aria-label=\"Delete &quot;db&quot;\""),
			"{html}"
		);
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(html.contains("type=\"button\""), "{html}");
	}

	#[test]
	fn the_type_is_one_of_two_values() {
		let label = html! { "Save" };
		let html = button(Variant::Default, Size::Md, "reset\" onclick=\"x", &label).into_string();
		assert!(html.contains("type=\"button\""), "{html}");
		assert!(!html.contains("onclick"), "{html}");
		let html = button(Variant::Default, Size::Md, "submit", &label).into_string();
		assert!(html.contains("type=\"submit\""), "{html}");
	}

	#[test]
	fn a_link_button_escapes_its_href() {
		let label = html! { "Open" };
		let html = link_button(Variant::Outline, Size::Md, "/a?b=\"c\"", &label).into_string();
		assert!(html.contains("href=\"/a?b=&quot;c&quot;\""), "{html}");
	}

	#[test]
	fn the_legacy_constants_are_buttons_too() {
		for c in [BUTTON, BUTTON_OUTLINE, BUTTON_DANGER] {
			assert!(c.starts_with("ui-button "), "{c}");
		}
	}
}
