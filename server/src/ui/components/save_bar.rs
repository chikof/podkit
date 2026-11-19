//! Save bar: the one place a settings form is saved from. It appears, sliding up from the
//! bottom edge of the page, only while the form differs from what it was when the page
//! loaded, says "Unsaved changes" and offers Discard and Save. While the form is clean no
//! save button is on screen at all.
//!
//! The form lists the Datastar signals its fields are bound to (`data-bind:<name>`) and puts
//! the bar as the last thing inside the `<form>`:
//!
//! ```ignore
//! form "data-on:submit"=(save) { /* fields */ (save_bar(&["name", "slug"])) }
//! ```
//!
//! - The bar snapshots those signals into the client-only `_initial` signal once the page
//!   is ready (`data-effect`), and the bar is dirty (`data-dirty` attribute, which the CSS
//!   shows it by) while any signal differs from the snapshot.
//! - Discard writes the snapshot back into the signals.
//! - After a successful save the handler adds [`saved`] to its response, which empties
//!   `_initial`; the effect then takes a new snapshot of what was just saved and the bar
//!   goes away. A handler that redirects or reloads needs nothing.
//! - `guard.js` (loaded by the bar) reads `[data-dirty]` to ask before the page is left (`beforeunload` and
//!   in-app links), so there is one definition of "unsaved" for both.
//!
//! One bar per page: the snapshot signal is named `_initial`.

use maud::{Markup, html};
use serde_json::json;

use super::button::{Size, Variant, button_class};
use crate::ui::assets::guard_script;
use crate::ui::layout::form_error;
use crate::ui::sse::{self, Events};

/// A signal name is a JS identifier; a wrong one is a bug in the page, not user input.
fn checked<'a>(fields: &[&'a str]) -> Vec<&'a str> {
	assert!(
		fields
			.iter()
			.all(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
		"save_bar fields are signal names: {fields:?}"
	);
	fields.to_vec()
}

/// The bar for a form whose fields are bound to the signals `fields`. Put it inside the
/// `<form>`, after the last field; its Save button submits that form.
#[must_use]
pub fn save_bar(fields: &[&str]) -> Markup {
	let fields = checked(fields);
	let join = |sep: &str, f: &dyn Fn(&str) -> String| {
		fields.iter().map(|n| f(n)).collect::<Vec<_>>().join(sep)
	};
	let snapshot = format!(
		"$_initial === '' && ($_initial = {{{}}})",
		join(", ", &|n| format!("{n}: ${n}"))
	);
	let dirty = format!(
		"$_initial !== '' && ({})",
		join(" || ", &|n| format!("${n} !== $_initial.{n}"))
	);
	let discard = format!(
		"{}; $_error = ''",
		join("; ", &|n| format!("${n} = $_initial.{n}"))
	);
	html! {
		div class="ui-save-bar" data-save-bar "data-signals"="{_initial: ''}" "data-effect"=(snapshot)
				"data-attr:data-dirty"=(dirty) {
			(guard_script())
			div class="ui-save-msg" {
				span class="ui-save-title" { "Unsaved changes" }
				(form_error())
			}
			div class="ui-save-act" {
				button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(discard) { "Discard" }
				button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save" }
			}
		}
	}
}

/// What a successful save adds to its response: the saved values become the new baseline,
/// so the bar goes away.
#[must_use]
pub fn saved() -> Events {
	sse::signals(&json!({ "_initial": "" }))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_bar_compares_every_field_with_its_snapshot() {
		let html = save_bar(&["name", "port"]).into_string();
		assert!(html.contains("class=\"ui-save-bar\""), "{html}");
		assert!(
			html.contains("$name !== $_initial.name || $port !== $_initial.port"),
			"{html}"
		);
		assert!(
			html.contains("$_initial = {name: $name, port: $port}"),
			"{html}"
		);
		assert!(
			html.contains("$name = $_initial.name; $port = $_initial.port"),
			"{html}"
		);
		assert!(html.contains("Unsaved changes"), "{html}");
	}

	#[test]
	fn a_clean_bar_is_not_shown_by_the_markup() {
		// Shown only by the `data-dirty` attribute the page sets client-side: the server
		// never renders it, and the stylesheet hides a bar without it.
		let html = save_bar(&["name"]).into_string();
		assert!(!html.contains(" data-dirty"), "{html}");
		let css = include_str!("../components.css");
		assert!(
			css.contains(".ui-save-bar:not([data-dirty]) { display: none;"),
			"the stylesheet must hide a clean bar"
		);
	}

	#[test]
	#[should_panic(expected = "signal names")]
	fn a_field_name_cannot_carry_script() {
		let _ = save_bar(&["a'); alert(1); ('"]);
	}
}
