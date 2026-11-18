//! Form controls: input, textarea, native select, checkbox, switch, label, help and error
//! text.
//!
//! A control that carries Datastar attributes (`data-bind:email`, ...) is written in the
//! page with the class constants:
//!
//! ```ignore
//! label class=(LABEL) { "Email" input class=(INPUT) type="email" "data-bind:email"; }
//! ```
//!
//! The `*_field` functions render the whole group (label, control, help, error, wired with
//! `aria-describedby`) for plain server-rendered forms.
//!
//! Validation: a field turns red through `:user-invalid`, so only after the user touched
//! it, never on load. An error the server found is rendered with `error_text` and
//! `aria-invalid`. A password field never echoes a value (secrets are write-only).

use maud::{Markup, html};

pub const INPUT: &str = "ui-field h-9 w-full min-w-0 rounded-md border border-input bg-transparent px-3 text-sm font-normal shadow-xs transition-colors outline-hidden placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50";

/// A `<label>` wrapping its text and control, stacked.
pub const LABEL: &str = "flex flex-col gap-2 text-sm leading-none font-medium";

/// A multi-line field for pasted text such as a private key.
pub const TEXTAREA: &str = "ui-field min-h-24 w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-sm font-normal shadow-xs transition-colors outline-hidden placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50";

/// A native `<select>`. With `appearance: base-select` support it is restyled; elsewhere
/// the platform picker stays. Put `<button><selectedcontent></selectedcontent></button>`
/// first inside it (`select_field` does).
pub const SELECT: &str = "ui-field ui-select h-9 w-full min-w-0 rounded-md border border-input bg-transparent px-3 text-sm font-normal shadow-xs transition-colors outline-hidden focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50";

pub const CHECKBOX: &str =
	"ui-checkbox outline-hidden focus-visible:ring-2 focus-visible:ring-ring/50";

pub const SWITCH: &str = "ui-switch outline-hidden focus-visible:ring-2 focus-visible:ring-ring/50";

const FIELD_LABEL: &str = "text-sm leading-none font-medium";

/// Muted text under a control. Link it with `aria-describedby`.
pub fn help_text(id: &str, text: &str) -> Markup {
	html! { p id=(id) class="text-sm text-muted-foreground" { (text) } }
}

/// An error the server found, announced when it appears.
pub fn error_text(id: &str, text: &str) -> Markup {
	html! { p id=(id) class="text-sm text-destructive" role="alert" { (text) } }
}

/// `aria-describedby` for the help and error text that exist, or none.
fn described_by(id: &str, help: Option<&str>, error: Option<&str>) -> Option<String> {
	let mut ids = Vec::new();
	if help.is_some() {
		ids.push(format!("{id}-help"));
	}
	if error.is_some() {
		ids.push(format!("{id}-error"));
	}
	(!ids.is_empty()).then(|| ids.join(" "))
}

fn messages(id: &str, help: Option<&str>, error: Option<&str>) -> Markup {
	html! {
		@if let Some(help) = help { (help_text(&format!("{id}-help"), help)) }
		@if let Some(error) = error { (error_text(&format!("{id}-error"), error)) }
	}
}

fn input_type(kind: &str) -> &'static str {
	match kind {
		"email" => "email",
		"password" => "password",
		"url" => "url",
		"number" => "number",
		"search" => "search",
		"tel" => "tel",
		_ => "text",
	}
}

/// A labelled single-line input. `kind` is an `<input type>` from a short list
/// (`text`, `email`, `password`, `url`, `number`, `search`, `tel`; anything else is `text`).
pub fn text_field(
	id: &str,
	name: &str,
	label: &str,
	kind: &str,
	value: Option<&str>,
	help: Option<&str>,
	error: Option<&str>,
) -> Markup {
	let kind = input_type(kind);
	// A secret is never written back into a page.
	let value = value.filter(|_| kind != "password");
	html! {
		div class="grid gap-2" {
			label for=(id) class=(FIELD_LABEL) { (label) }
			input id=(id) name=(name) type=(kind) class=(INPUT) value=[value]
				aria-describedby=[described_by(id, help, error)]
				aria-invalid=[error.map(|_| "true")];
			(messages(id, help, error))
		}
	}
}

pub fn textarea_field(
	id: &str,
	name: &str,
	label: &str,
	value: Option<&str>,
	help: Option<&str>,
	error: Option<&str>,
) -> Markup {
	html! {
		div class="grid gap-2" {
			label for=(id) class=(FIELD_LABEL) { (label) }
			textarea id=(id) name=(name) class=(TEXTAREA)
				aria-describedby=[described_by(id, help, error)]
				aria-invalid=[error.map(|_| "true")] { (value.unwrap_or_default()) }
			(messages(id, help, error))
		}
	}
}

/// A labelled `<select>` over `(value, text)` options. `selected` is a value.
pub fn select_field(
	id: &str,
	name: &str,
	label: &str,
	options: &[(&str, &str)],
	selected: Option<&str>,
	help: Option<&str>,
	error: Option<&str>,
) -> Markup {
	html! {
		div class="grid gap-2" {
			label for=(id) class=(FIELD_LABEL) { (label) }
			select id=(id) name=(name) class=(SELECT)
				aria-describedby=[described_by(id, help, error)]
				aria-invalid=[error.map(|_| "true")] {
				button { selectedcontent {} }
				@for (value, text) in options {
					option value=(value) selected[selected == Some(*value)] { (text) }
				}
			}
			(messages(id, help, error))
		}
	}
}

fn toggle_field(
	id: &str,
	name: &str,
	label: &str,
	checked: bool,
	help: Option<&str>,
	switch: bool,
) -> Markup {
	html! {
		div class="grid gap-1.5" {
			div class="flex items-center gap-3" {
				@if switch {
					input id=(id) name=(name) type="checkbox" role="switch" class=(SWITCH) checked[checked]
						aria-describedby=[described_by(id, help, None)];
				} @else {
					input id=(id) name=(name) type="checkbox" class=(CHECKBOX) checked[checked]
						aria-describedby=[described_by(id, help, None)];
				}
				label for=(id) class="cursor-pointer text-sm leading-none font-medium" { (label) }
			}
			@if let Some(help) = help {
				div class="pl-7" { (help_text(&format!("{id}-help"), help)) }
			}
		}
	}
}

pub fn checkbox_field(
	id: &str,
	name: &str,
	label: &str,
	checked: bool,
	help: Option<&str>,
) -> Markup {
	toggle_field(id, name, label, checked, help, false)
}

/// A switch: a checkbox that applies at once. Use it for a setting, not for a form answer.
pub fn switch_field(
	id: &str,
	name: &str,
	label: &str,
	checked: bool,
	help: Option<&str>,
) -> Markup {
	toggle_field(id, name, label, checked, help, true)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_text_field_labels_describes_and_flags_its_error() {
		let html = text_field(
			"email",
			"email",
			"Email",
			"email",
			Some("a@b.c"),
			Some("We never share it."),
			Some("Not an address."),
		)
		.into_string();
		assert!(html.contains("<label for=\"email\""), "{html}");
		assert!(
			html.contains("aria-describedby=\"email-help email-error\""),
			"{html}"
		);
		assert!(html.contains("aria-invalid=\"true\""), "{html}");
		assert!(
			html.contains("id=\"email-error\" class=\"text-sm text-destructive\" role=\"alert\""),
			"{html}"
		);
		assert!(html.contains("value=\"a@b.c\""), "{html}");
	}

	#[test]
	fn a_field_without_messages_has_no_aria_noise() {
		let html = text_field("n", "n", "Name", "text", None, None, None).into_string();
		assert!(!html.contains("aria-describedby"), "{html}");
		assert!(!html.contains("aria-invalid"), "{html}");
		assert!(!html.contains("value="), "{html}");
	}

	#[test]
	fn a_password_is_never_echoed_and_an_odd_type_is_text() {
		let html = text_field(
			"p",
			"p",
			"Password",
			"password",
			Some("hunter2"),
			None,
			None,
		)
		.into_string();
		assert!(html.contains("type=\"password\""), "{html}");
		assert!(!html.contains("hunter2"), "{html}");
		let html = text_field("p", "p", "P", "text\" onfocus=\"x", None, None, None).into_string();
		assert!(html.contains("type=\"text\""), "{html}");
		assert!(!html.contains("onfocus"), "{html}");
	}

	#[test]
	fn values_and_messages_are_escaped() {
		let html = text_field(
			"x",
			"x",
			"<b>L</b>",
			"text",
			Some("\"><script>alert(1)</script>"),
			Some("<i>h</i>"),
			Some("<u>e</u>"),
		)
		.into_string();
		for bad in ["<script>", "<b>", "<i>", "<u>"] {
			assert!(!html.contains(bad), "{bad} in {html}");
		}
	}

	#[test]
	fn a_textarea_escapes_its_content() {
		let html = textarea_field(
			"k",
			"k",
			"Key",
			Some("</textarea><script>x</script>"),
			None,
			None,
		)
		.into_string();
		assert!(!html.contains("</textarea><script>"), "{html}");
		assert!(html.contains("&lt;/textarea&gt;"), "{html}");
	}

	#[test]
	fn a_select_marks_the_chosen_option_and_has_the_customizable_button() {
		let html = select_field(
			"r",
			"region",
			"Region",
			&[("eu", "Europe"), ("us", "United <States>")],
			Some("us"),
			None,
			None,
		)
		.into_string();
		assert!(
			html.contains("<button><selectedcontent></selectedcontent></button>"),
			"{html}"
		);
		assert!(
			html.contains("<option value=\"us\" selected>United &lt;States&gt;</option>"),
			"{html}"
		);
		assert!(
			html.contains("<option value=\"eu\">Europe</option>"),
			"{html}"
		);
	}

	#[test]
	fn a_switch_has_the_switch_role_and_a_checkbox_does_not() {
		let on = switch_field("s", "s", "Auto deploy", true, Some("On every push.")).into_string();
		assert!(on.contains("role=\"switch\""), "{on}");
		assert!(on.contains("checked"), "{on}");
		assert!(on.contains("aria-describedby=\"s-help\""), "{on}");
		let off = checkbox_field("c", "c", "Remember", false, None).into_string();
		assert!(!off.contains("role=\"switch\""), "{off}");
		assert!(!off.contains("checked"), "{off}");
	}
}
