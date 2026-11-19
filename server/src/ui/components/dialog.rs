//! Dialog: the native `<dialog>`, opened as a modal by an invoker button
//! (`command="show-modal" commandfor=<id>`). The browser makes the page inert, traps and
//! returns focus, closes on Esc and styles `::backdrop`; there is no focus-trap code here
//! and no `role="dialog"` or `aria-modal` (a native modal already has both).
//!
//! Browsers without invoker commands get the same open through a Datastar fallback on the
//! button (`data-on:click`) and through `page.js`; both do nothing where the command already
//! worked.
//!
//! Every dialog uses `closedby="any"`: Esc and a click outside both close it (`page.js` does
//! the click outside where the attribute is unsupported). That includes a dialog with a
//! form. When something typed in it has not been submitted, `page.js` holds the close back
//! and asks "Discard changes?" (`discard_confirm`), so a stray click cannot lose input; the
//! dialog's own Close and Cancel buttons are deliberate and close at once.
//!
//! Put `autofocus` on the safe action (Cancel, Keep) of a destructive confirmation, or on
//! the first field of a form. Close with `dialog_cancel` (a `<form method="dialog">`) or
//! any `<form method="dialog">` button.

use maud::{Markup, html};

use super::button::{Size, Variant, button_class};
use super::icon::icon;
use crate::ui::sse::js_string;

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
	body: &Markup,
	footer: &Markup,
) -> Markup {
	let title_id = format!("{id}-title");
	let description_id = format!("{id}-description");
	html! {
		dialog id=(id) class="ui-dialog" closedby="any" aria-labelledby=(title_id)
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

/// The "Discard changes?" question `page.js` asks before a dialog with unsaved input is
/// closed by Esc or a click outside, or before the page is left with unsaved changes. One
/// per page, in the shell; it answers through its `returnValue` (`keep` or `discard`).
pub fn discard_confirm() -> Markup {
	html! {
		dialog id="discard-confirm" class="ui-dialog" closedby="any"
			aria-labelledby="discard-confirm-title" aria-describedby="discard-confirm-description" {
			div class="ui-dialog-head" {
				div {
					h2 id="discard-confirm-title" class="ui-dialog-title" { "Discard changes?" }
					p id="discard-confirm-description" class="ui-dialog-desc" { "What you changed has not been saved." }
				}
			}
			div class="ui-dialog-foot" {
				form method="dialog" { button value="keep" class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep editing" } }
				form method="dialog" { button value="discard" class=(button_class(Variant::Destructive, Size::Md)) { "Discard" } }
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> String {
		dialog(
			"del",
			"Delete \"api\"?",
			Some("This cannot be undone."),
			&html! { "Type the name." },
			&html! { (dialog_cancel("b", &html! { "Cancel" })) },
		)
		.into_string()
	}

	#[test]
	fn a_dialog_is_named_and_described_without_redundant_roles() {
		let html = sample();
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
	fn every_dialog_closes_on_a_click_outside() {
		assert!(sample().contains("closedby=\"any\""));
		assert!(!sample().contains("closerequest"));
		assert!(discard_confirm().into_string().contains("closedby=\"any\""));
	}

	#[test]
	fn no_dialog_in_the_ui_opts_out_of_light_dismiss() {
		// A `<dialog>` written by hand in a page must say `closedby="any"` too.
		fn walk(dir: &std::path::Path, bad: &mut Vec<String>) {
			for entry in std::fs::read_dir(dir).unwrap() {
				let path = entry.unwrap().path();
				if path.is_dir() {
					walk(&path, bad);
				} else if path.extension().is_some_and(|e| e == "rs") {
					let src = std::fs::read_to_string(&path).unwrap();
					for needle in [
						"closedby=\"closerequest\"",
						"closedby=\"none\"",
						"closedby=(\"",
						"Dismiss::",
					] {
						// This test names them; every other file must not.
						if !path.ends_with("dialog.rs") && src.contains(needle) {
							bad.push(format!("{}: {needle}", path.display()));
						}
					}
				}
			}
		}
		let mut bad = Vec::new();
		walk(
			&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui"),
			&mut bad,
		);
		assert!(bad.is_empty(), "{bad:?}");
	}

	#[test]
	fn a_dialog_without_a_description_has_no_describedby() {
		let html = dialog("d", "T", None, &html! {}, &html! {}).into_string();
		assert!(!html.contains("aria-describedby"), "{html}");
	}

	#[test]
	fn the_title_is_escaped_and_the_close_button_is_named() {
		let html = dialog("d", "<script>x</script>", None, &html! {}, &html! {}).into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("aria-label=\"Close\""), "{html}");
		assert!(html.contains("<form method=\"dialog\""), "{html}");
	}

	#[test]
	fn the_discard_question_offers_keep_first_and_discard() {
		let html = discard_confirm().into_string();
		assert!(html.contains("Discard changes?"), "{html}");
		assert!(html.contains("value=\"keep\""), "{html}");
		assert!(html.contains("value=\"discard\""), "{html}");
		// The safe answer holds focus: `autofocus` sits between "keep" and "discard".
		let keep = html.find("value=\"keep\"").unwrap();
		let discard = html.find("value=\"discard\"").unwrap();
		let focus = html.find("autofocus").unwrap();
		assert!(keep < focus && focus < discard, "{html}");
		assert_eq!(html.matches("autofocus").count(), 1, "{html}");
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
