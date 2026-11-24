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
	bar(fields, "_initial", "_busy", "_error")
}

/// The bar of a page with more than one edit form: each form names its `scope`, which gives
/// it its own snapshot (`_initial_<scope>`), busy (`_<scope>_busy`) and error
/// (`_<scope>_error`) signals. The handler that fails sets the error signal; one that succeeds
/// redirects, or adds [`saved_in`].
#[must_use]
pub fn save_bar_in(scope: &str, fields: &[&str]) -> Markup {
	checked(&[scope]);
	bar(
		fields,
		&format!("_initial_{scope}"),
		&format!("_{scope}_busy"),
		&format!("_{scope}_error"),
	)
}

fn bar(fields: &[&str], initial: &str, busy: &str, error: &str) -> Markup {
	let fields = checked(fields);
	let join = |sep: &str, f: &dyn Fn(&str) -> String| {
		fields.iter().map(|n| f(n)).collect::<Vec<_>>().join(sep)
	};
	let snapshot = format!(
		"${initial} === '' && (${initial} = {{{}}})",
		join(", ", &|n| format!("{n}: ${n}"))
	);
	let dirty = format!(
		"${initial} !== '' && ({})",
		join(" || ", &|n| format!("${n} !== ${initial}.{n}"))
	);
	let discard = format!(
		"{}; ${error} = ''",
		join("; ", &|n| format!("${n} = ${initial}.{n}"))
	);
	let signals = format!("{{{initial}: ''}}");
	html! {
		div class="ui-save-bar" data-save-bar "data-signals"=(signals) "data-effect"=(snapshot)
				"data-attr:data-dirty"=(dirty) {
			(guard_script())
			div class="ui-save-msg" {
				span class="ui-save-title" { "Unsaved changes" }
				p class="text-sm text-destructive" role="alert" "data-show"={ "$" (error) } "data-text"={ "$" (error) } style="display: none" {}
			}
			div class="ui-save-act" {
				button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(discard) { "Discard" }
				button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator"=(busy) "data-attr:disabled"={ "$" (busy) } { "Save" }
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

/// [`saved`] for the bar of `scope` (see [`save_bar_in`]).
#[must_use]
pub fn saved_in(scope: &str) -> Events {
	checked(&[scope]);
	sse::signals(&json!({ format!("_initial_{scope}"): "" }))
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
	fn a_scoped_bar_has_signals_of_its_own() {
		let a = save_bar_in("smtp", &["host"]).into_string();
		let b = save_bar_in("nz", &["dedup"]).into_string();
		for (html, scope, field) in [(&a, "smtp", "host"), (&b, "nz", "dedup")] {
			assert!(
				html.contains(&format!("$_initial_{scope} !== ''")),
				"{html}"
			);
			assert!(
				html.contains(&format!("${field} !== $_initial_{scope}.{field}")),
				"{html}"
			);
			assert!(
				html.contains(&format!("{{_initial_{scope}: ''}}")),
				"{html}"
			);
			assert!(
				html.contains(&format!("data-indicator=\"_{scope}_busy\"")),
				"{html}"
			);
			assert!(
				html.contains(&format!("data-attr:disabled=\"$_{scope}_busy\"")),
				"{html}"
			);
			assert!(
				html.contains(&format!("data-show=\"$_{scope}_error\"")),
				"{html}"
			);
		}
		assert!(!a.contains("_initial_nz") && !b.contains("_initial_smtp"));
		assert!(!a.contains("$_initial ") && !a.contains("$_busy"), "{a}");
		let _ = saved_in("smtp");
	}

	#[test]
	fn the_default_bar_keeps_the_page_wide_signals() {
		let html = save_bar(&["name"]).into_string();
		assert!(html.contains("data-indicator=\"_busy\""), "{html}");
		assert!(html.contains("data-attr:disabled=\"$_busy\""), "{html}");
		assert!(html.contains("data-show=\"$_error\""), "{html}");
		assert!(html.contains("{_initial: ''}"), "{html}");
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
