//! Form controls: input, textarea, native select, checkbox, switch, password and copy
//! fields, label, hint and error text.
//!
//! A control that carries Datastar attributes (`data-bind:email`, ...) is written in the
//! page with the class constants:
//!
//! ```ignore
//! label class=(LABEL) { "Email" input class=(INPUT) type="email" "data-bind:email"; }
//! ```
//!
//! The `*_field` functions render the whole group (label, control, hint, error, wired with
//! `aria-describedby`) for plain server-rendered forms.
//!
//! Validation: a field turns red through `:user-invalid`, so only after the user touched
//! it, never on load. An error the server found is rendered with `error_text` and
//! `aria-invalid`; a message that belongs to the browser's own check is `invalid_text`,
//! which shows only while its field is invalid. A password field never echoes a value
//! (secrets are write-only).

use maud::{Markup, html};

use super::button::{Size, Variant, button_class};
use super::icon::icon;

pub const INPUT: &str = "ui-field";

/// A `<label>` wrapping its text and control, stacked.
pub const LABEL: &str = "ui-label";

/// A multi-line field for pasted text such as a private key.
pub const TEXTAREA: &str = "ui-field ui-textarea font-mono text-sm";

/// A native `<select>`. With `appearance: base-select` support it is restyled; elsewhere
/// the platform picker stays. Put `<button><selectedcontent></selectedcontent></button>`
/// first inside it (`select_field` does) and wrap it in `ui-select-wrap` with a chevron
/// (`select_field` does that too).
pub const SELECT: &str = "ui-field ui-select";

pub const CHECKBOX: &str = "ui-checkbox";

pub const SWITCH: &str = "ui-switch";

/// Shows or hides the password in the input before the button. The state is
/// `aria-pressed`, and the name of the button follows it.
static REVEAL: &str = "const i = el.previousElementSibling; const show = i.type === 'password'; i.type = show ? 'text' : 'password'; el.setAttribute('aria-pressed', show); el.setAttribute('aria-label', show ? 'Hide password' : 'Show password')";

/// The button that reveals the password in the input before it (a bound input on the
/// sign-in and sign-up forms; `password_field` is the plain-form version).
pub fn reveal_button() -> Markup {
	html! {
		button type="button" class="ui-pw-btn" aria-label="Show password" aria-pressed="false" "data-on:click"=(REVEAL) {
			(icon("eye"))
			(icon("eye-off"))
		}
	}
}

/// Copies the value of the input before the button. The Clipboard API is withheld over
/// plain HTTP, so the fallback copies through the selected input.
static COPY: &str = "const i = el.previousElementSibling; const done = () => { const s = el.querySelector('span'); s.textContent = 'Copied'; setTimeout(() => s.textContent = 'Copy', 1600) }; if (window.isSecureContext && navigator.clipboard) { navigator.clipboard.writeText(i.value).then(done) } else { i.select(); document.execCommand('copy'); done() }";

/// Muted text under a control. Link it with `aria-describedby`.
pub fn help_text(id: &str, text: &str) -> Markup {
	html! { p id=(id) class="ui-hint" { (text) } }
}

/// An error the server found, announced when it appears.
pub fn error_text(id: &str, text: &str) -> Markup {
	html! { p id=(id) class="ui-error" role="alert" { (icon("circle-alert")) span { (text) } } }
}

/// An error that shows only while its field is invalid (`:user-invalid` or `aria-invalid`),
/// for the browser's own checks (`required`, `type=email`, `pattern`). Put it right after the
/// control, inside the field group.
pub fn invalid_text(id: &str, text: &str) -> Markup {
	html! { p id=(id) class="ui-error" data-when="invalid" { (icon("circle-alert")) span { (text) } } }
}

/// `aria-describedby` for the hint and error text that exist, or none.
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
		div class="ui-field-group" {
			label for=(id) class=(LABEL) { (label) }
			input id=(id) name=(name) type=(kind) class=(INPUT) value=[value]
				aria-describedby=[described_by(id, help, error)]
				aria-invalid=[error.map(|_| "true")];
			(messages(id, help, error))
		}
	}
}

/// A password input with a button that reveals it (the button is named and `aria-pressed`).
/// The field has no `value`: a secret is never written back into a page.
pub fn password_field(
	id: &str,
	name: &str,
	label: &str,
	autocomplete: &str,
	help: Option<&str>,
	error: Option<&str>,
) -> Markup {
	let autocomplete = if autocomplete == "new-password" {
		"new-password"
	} else {
		"current-password"
	};
	html! {
		div class="ui-field-group" {
			label for=(id) class=(LABEL) { (label) }
			div class="ui-pw" {
				input id=(id) name=(name) type="password" class=(INPUT) autocomplete=(autocomplete)
					aria-describedby=[described_by(id, help, error)]
					aria-invalid=[error.map(|_| "true")];
				button type="button" class="ui-pw-btn" aria-label="Show password" aria-pressed="false"
					"data-on:click"=(REVEAL) {
					(icon("eye"))
					(icon("eye-off"))
				}
			}
			(messages(id, help, error))
		}
	}
}

/// A read-only value with a Copy button: a connection string, a webhook URL, a host.
/// Never use it for a secret the server holds write-only.
pub fn copy_field(id: &str, label: &str, value: &str) -> Markup {
	html! {
		div class="ui-field-group" {
			label for=(id) class=(LABEL) { (label) }
			div class="ui-copy" {
				input id=(id) type="text" class=(INPUT) value=(value) readonly;
				button type="button" class=(button_class(Variant::Outline, Size::Md))
					aria-label={ "Copy " (label.to_lowercase()) } "data-on:click"=(COPY) {
					(icon("copy"))
					span { "Copy" }
				}
			}
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
		div class="ui-field-group" {
			label for=(id) class=(LABEL) { (label) }
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
		div class="ui-field-group" {
			label for=(id) class=(LABEL) { (label) }
			div class="ui-select-wrap" {
				select id=(id) name=(name) class=(SELECT)
					aria-describedby=[described_by(id, help, error)]
					aria-invalid=[error.map(|_| "true")] {
					button { selectedcontent {} }
					@for (value, text) in options {
						option value=(value) selected[selected == Some(*value)] { (text) }
					}
				}
				(icon("chevron-down"))
			}
			(messages(id, help, error))
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
	html! {
		div class="ui-field-group" {
			div class="ui-check" {
				input id=(id) name=(name) type="checkbox" class=(CHECKBOX) checked[checked]
					aria-describedby=[described_by(id, help, None)];
				label for=(id) class="cursor-pointer" { (label) }
			}
			@if let Some(help) = help {
				div class="pl-6" { (help_text(&format!("{id}-help"), help)) }
			}
		}
	}
}

/// A switch: a checkbox that applies at once. Use it for a setting, not for a form answer.
/// The native `switch` attribute gives it its role where the browser knows it; the painted
/// track and thumb are the same everywhere. Title and help sit beside it, the switch at the end.
pub fn switch_field(
	id: &str,
	name: &str,
	label: &str,
	checked: bool,
	help: Option<&str>,
) -> Markup {
	html! {
		div class="ui-check-row" {
			input id=(id) name=(name) type="checkbox" switch role="switch" class=(SWITCH) checked[checked]
				aria-describedby=[described_by(id, help, None)];
			div {
				label for=(id) class="ui-check-title" { (label) }
				@if let Some(help) = help {
					small id={ (id) "-help" } { (help) }
				}
			}
		}
	}
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
			html.contains("id=\"email-error\" class=\"ui-error\" role=\"alert\""),
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
	fn the_reveal_button_is_named_pressed_and_the_input_has_no_value() {
		let html = password_field(
			"pw",
			"pw",
			"Password",
			"new-password",
			None,
			Some("Too short."),
		)
		.into_string();
		assert!(html.contains("aria-label=\"Show password\""), "{html}");
		assert!(html.contains("aria-pressed=\"false\""), "{html}");
		assert!(html.contains("autocomplete=\"new-password\""), "{html}");
		assert!(html.contains("aria-invalid=\"true\""), "{html}");
		assert!(!html.contains("value="), "{html}");
		let odd = password_field("pw", "pw", "P", "x\" onfocus=\"y", None, None).into_string();
		assert!(odd.contains("autocomplete=\"current-password\""), "{odd}");
		assert!(!odd.contains("onfocus"), "{odd}");
	}

	#[test]
	fn a_copy_field_is_read_only_and_escapes_its_value() {
		let html =
			copy_field("dsn", "Connection string", "postgres://u:\"><script>@h/db").into_string();
		assert!(html.contains("readonly"), "{html}");
		assert!(!html.contains("<script>"), "{html}");
		assert!(
			html.contains("aria-label=\"Copy connection string\""),
			"{html}"
		);
		assert!(html.contains("navigator.clipboard"), "{html}");
		assert!(html.contains("execCommand('copy')"), "{html}");
	}

	#[test]
	fn an_invalid_message_shows_only_for_an_invalid_field() {
		let html = invalid_text("e-invalid", "Enter an address.").into_string();
		assert!(html.contains("data-when=\"invalid\""), "{html}");
		assert!(!html.contains("role=\"alert\""), "{html}");
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
		assert!(html.contains("ui-select-wrap"), "{html}");
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
		assert!(on.contains(" switch "), "{on}");
		assert!(on.contains("checked"), "{on}");
		assert!(on.contains("aria-describedby=\"s-help\""), "{on}");
		assert!(
			on.contains("<small id=\"s-help\">On every push.</small>"),
			"{on}"
		);
		let off = checkbox_field("c", "c", "Remember", false, None).into_string();
		assert!(!off.contains("role=\"switch\""), "{off}");
		assert!(!off.contains("checked"), "{off}");
	}
}
