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
	/// The main action of a view.
	Default,
	/// A supporting action next to the main one.
	Secondary,
	/// A bordered action; the usual choice for the second button.
	Outline,
	/// A low-emphasis action, e.g. in a toolbar or a row.
	Ghost,
	/// An action that deletes or stops something.
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

const BASE: &str = "ui-button inline-flex shrink-0 cursor-pointer items-center justify-center gap-2 rounded-md border text-sm font-medium whitespace-nowrap transition-colors outline-hidden select-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50";

const DEFAULT: &str = "border-transparent bg-primary text-primary-foreground shadow-xs hover:bg-primary/90 active:bg-primary/80";
const SECONDARY: &str = "border-transparent bg-secondary text-secondary-foreground hover:bg-secondary/80 active:bg-secondary/70";
const OUTLINE: &str = "border-input bg-background text-foreground shadow-xs hover:bg-accent hover:text-accent-foreground active:bg-accent/70";
const GHOST: &str = "border-transparent text-foreground hover:bg-accent hover:text-accent-foreground active:bg-accent/70";
const DESTRUCTIVE: &str = "border-transparent bg-destructive text-destructive-foreground shadow-xs hover:bg-destructive/90 active:bg-destructive/80";
const LINK: &str =
	"border-transparent text-primary underline-offset-4 hover:underline active:opacity-80";

const SM: &str = "h-8 px-3 text-xs";
const MD: &str = "h-9 px-4";
const LG: &str = "h-10 px-6";
const ICON: &str = "h-9 w-9";

/// The main action on a page.
pub const BUTTON: &str = "ui-button inline-flex h-9 shrink-0 cursor-pointer items-center justify-center gap-2 rounded-md border border-transparent bg-primary px-4 text-sm font-medium whitespace-nowrap text-primary-foreground shadow-xs transition-colors outline-hidden select-none hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-ring/50 active:bg-primary/80 disabled:pointer-events-none disabled:opacity-50";

/// A secondary action, drawn as a bordered button.
pub const BUTTON_OUTLINE: &str = "ui-button inline-flex h-9 shrink-0 cursor-pointer items-center justify-center gap-2 rounded-md border border-input bg-background px-4 text-sm font-medium whitespace-nowrap text-foreground shadow-xs transition-colors outline-hidden select-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring/50 active:bg-accent/70 disabled:pointer-events-none disabled:opacity-50";

/// A destructive action drawn as a bordered button in the error color: quieter than
/// `Variant::Destructive`, for a delete next to other actions in a row or card.
pub const BUTTON_DANGER: &str = "ui-button inline-flex h-9 shrink-0 cursor-pointer items-center justify-center gap-2 rounded-md border border-input bg-background px-4 text-sm font-medium whitespace-nowrap text-destructive shadow-xs transition-colors outline-hidden select-none hover:bg-destructive/10 focus-visible:ring-2 focus-visible:ring-ring/50 active:bg-destructive/20 disabled:pointer-events-none disabled:opacity-50";

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
	format!("{BASE} {variant} {size}")
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
		assert!(button_class(Variant::Default, Size::Icon).contains("h-9 w-9"));
		assert!(button_class(Variant::Default, Size::Sm).contains("h-8"));
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
			assert!(c.contains("ui-button") && c.contains("focus-visible:ring-2"));
		}
	}
}
