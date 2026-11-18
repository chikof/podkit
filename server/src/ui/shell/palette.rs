//! The command palette: a native modal `<dialog>` holding a combobox.
//!
//! Ctrl/⌘-K or the header's search button opens it. Typing sends the text to
//! `GET /teams/{team}/search?q=` (debounced) and the answer, a Datastar patch, replaces
//! the listbox. Focus stays in the input; the arrow keys move the selected option
//! (`aria-activedescendant`), Enter opens it, Esc closes the dialog and focus goes back to
//! where it was. The key handling is in `podkit.js`, section 5.

use maud::{Markup, html};

use crate::ui::components::{icon, kbd};
use crate::ui::extract::TeamCtx;
use crate::ui::pages::search::{LIST_ID, defaults};

/// The expression that opens or closes the palette from the keyboard. It leaves other
/// open dialogs alone, so a confirmation is never buried.
pub const HOTKEY: &str = "(evt.ctrlKey || evt.metaKey) && !evt.altKey && evt.key.toLowerCase() === 'k' && (evt.preventDefault(), document.getElementById('palette').open ? document.getElementById('palette').close() : document.querySelector('dialog[open]') || document.getElementById('palette').showModal())";

pub fn dialog(ctx: &TeamCtx) -> Markup {
	let on_input = format!(
		"@get('/teams/{}/search?q=' + encodeURIComponent(el.value))",
		ctx.team.id
	);
	html! {
		dialog id="palette" class="ui-dialog ui-palette" closedby="any" aria-label="Search" {
			div class="ui-palette-bar" {
				(icon("search"))
				input type="text" role="combobox" autofocus data-palette-input
					aria-label="Search projects, resources, pages and actions"
					aria-expanded="true" aria-controls=(LIST_ID) aria-autocomplete="list"
					aria-activedescendant="palette-opt-0"
					autocomplete="off" autocapitalize="off" spellcheck="false" enterkeyhint="go"
					placeholder="Search projects, resources and actions…"
					class="ui-palette-input"
					"data-on:input__debounce.150ms"=(on_input);
				form method="dialog" {
					button class="ui-palette-close" aria-label="Close search" {
						span class="hidden sm:inline" aria-hidden="true" { (kbd("Esc")) }
						span class="sm:hidden" { (icon("x")) }
					}
				}
			}
			(defaults(ctx))
			div class="ui-palette-foot" aria-hidden="true" {
				span { (kbd("↑")) (kbd("↓")) " move" }
				span { (kbd("↵")) " open" }
				span { (kbd("Esc")) " close" }
			}
		}
	}
}
