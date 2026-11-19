//! Dialog: the native `<dialog>`, opened as a modal by an invoker button
//! (`command="show-modal" commandfor=<id>`). The browser makes the page inert, traps and
//! returns focus, closes on Esc and styles `::backdrop`; there is no focus-trap code here
//! and no `role="dialog"` or `aria-modal` (a native modal already has both).
//!
//! Browsers without invoker commands get the same open through a Datastar fallback on the
//! button (`data-on:click`) and through `page.js`; both do nothing where the command already
//! worked. `closedby="any"` (light dismiss) has a `page.js` fallback too; a form keeps
//! `closedby="closerequest"` (Esc only) so a stray click outside cannot lose what was typed.
//!
//! Put `autofocus` on the safe action (Cancel, Keep) of a destructive confirmation, or on
//! the first field of a form. Close with `dialog_cancel` (a `<form method="dialog">`) or
//! any `<form method="dialog">` button.

use maud::{Markup, html};

use super::icon::icon;
use crate::ui::sse::js_string;

/// How a dialog may be dismissed besides its own buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dismiss {
	/// Esc (and the back gesture) only. Use for confirmations and forms with input.
	Esc,
	/// Esc and a click outside. Use for read-only content.
	EscOrOutside,
}

/// The button that opens the dialog `id`. `class` is a button class list.
pub fn dialog_open_button(id: &str, class: &str, label: &Markup) -> Markup {
	let fallback = format!(
		"el.commandForElement || document.getElementById({}).showModal()",
		js_string(id)
	);
	html! {
		button type="button" class=(class) command="show-modal" commandfor=(id) aria-haspopup="dialog" "data-on:click"=(fallback) {
			(label)
		}
	}
}

/// A button that closes the dialog around it without submitting anything.
pub fn dialog_cancel(class: &str, label: &Markup) -> Markup {
	html! {
		form method="dialog" {
			button class=(class) { (label) }
		}
	}
}

/// A modal dialog named by `title`. `body` is the content, `footer` the actions (usually
/// a `dialog_cancel` and one confirming button).
pub fn dialog(
	id: &str,
	title: &str,
	description: Option<&str>,
	dismiss: Dismiss,
	body: &Markup,
	footer: &Markup,
) -> Markup {
	let title_id = format!("{id}-title");
	let description_id = format!("{id}-description");
	let closedby = match dismiss {
		Dismiss::Esc => "closerequest",
		Dismiss::EscOrOutside => "any",
	};
	html! {
		dialog id=(id) class="ui-dialog" closedby=(closedby) aria-labelledby=(title_id)
			aria-describedby=[description.map(|_| description_id.as_str())] {
			div class="ui-dialog-head" {
				div {
					h2 id=(title_id) class="ui-dialog-title" { (title) }
					@if let Some(description) = description {
						p id=(description_id) class="ui-dialog-desc" { (description) }
					}
				}
				form method="dialog" {
					button class="ui-icon-btn" aria-label="Close" { (icon("x")) }
				}
			}
			div class="ui-dialog-body" { (body) }
			@if !footer.0.is_empty() {
				div class="ui-dialog-foot" { (footer) }
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample(dismiss: Dismiss) -> String {
		dialog(
			"del",
			"Delete \"api\"?",
			Some("This cannot be undone."),
			dismiss,
			&html! { "Type the name." },
			&html! { (dialog_cancel("b", &html! { "Cancel" })) },
		)
		.into_string()
	}

	#[test]
	fn a_dialog_is_named_and_described_without_redundant_roles() {
		let html = sample(Dismiss::Esc);
		assert!(html.starts_with("<dialog id=\"del\""), "{html}");
		assert!(html.contains("aria-labelledby=\"del-title\""), "{html}");
		assert!(
			html.contains("aria-describedby=\"del-description\""),
			"{html}"
		);
		assert!(html.contains("<h2 id=\"del-title\""), "{html}");
		assert!(!html.contains("role=\"dialog\""), "{html}");
		assert!(!html.contains("aria-modal"), "{html}");
	}

	#[test]
	fn the_dismiss_mode_sets_closedby() {
		assert!(sample(Dismiss::Esc).contains("closedby=\"closerequest\""));
		assert!(sample(Dismiss::EscOrOutside).contains("closedby=\"any\""));
	}

	#[test]
	fn a_dialog_without_a_description_has_no_describedby() {
		let html = dialog("d", "T", None, Dismiss::Esc, &html! {}, &html! {}).into_string();
		assert!(!html.contains("aria-describedby"), "{html}");
	}

	#[test]
	fn the_title_is_escaped_and_the_close_button_is_named() {
		let html = dialog(
			"d",
			"<script>x</script>",
			None,
			Dismiss::Esc,
			&html! {},
			&html! {},
		)
		.into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("aria-label=\"Close\""), "{html}");
		assert!(html.contains("<form method=\"dialog\""), "{html}");
	}

	#[test]
	fn the_opener_uses_the_invoker_and_a_datastar_fallback_that_escapes_the_id() {
		let html = dialog_open_button("del", "c", &html! { "Delete" }).into_string();
		assert!(html.contains("command=\"show-modal\""), "{html}");
		assert!(html.contains("commandfor=\"del\""), "{html}");
		assert!(html.contains("aria-haspopup=\"dialog\""), "{html}");
		assert!(
			html.contains("el.commandForElement || document.getElementById('del').showModal()"),
			"{html}"
		);
		let nasty = dialog_open_button("');alert(1);//", "c", &html! {}).into_string();
		assert!(!nasty.contains("alert(1);//\")"), "{nasty}");
		assert!(!nasty.contains("getElementById('')"), "{nasty}");
	}
}
