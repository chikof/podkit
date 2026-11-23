//! Terminal: a shell in a running container, drawn by the vendored xterm.js and driven by
//! `terminal.js` over the WebSocket `/ws/terminal/{resource}`.
//!
//! The markup is the frame only: a status line, the tools (Copy, Paste, Full screen and, once
//! the session is over, Reconnect) and the empty screen the script fills. It works without
//! script only as far as saying so. The colors are `--term-*` tokens in `components.css`.
//!
//! The server decides who may open one; this component only builds the address, and the page
//! that shows it checks the `terminal` permission first so nobody is offered a dead button.

use maud::{Markup, html};

use crate::ui::assets::{XTERM_FIT_JS_HREF, XTERM_JS_HREF, terminal_assets};

const TOOL: &str = "ui-button ui-button-outline ui-button-sm";

/// What a terminal page needs to know.
pub struct Terminal<'a> {
	/// The accessible name of the screen: "Terminal in Storefront".
	pub label: &'a str,
	/// The WebSocket path with its query (team and project), starting with `/ws/terminal/`.
	pub socket: &'a str,
	/// The team keeps what its terminals print; the page says so before anyone types.
	pub recorded: bool,
}

/// The terminal and, once per page, its stylesheet and scripts.
pub fn terminal(t: &Terminal<'_>) -> Markup {
	html! {
		div class="ui-terminal" data-terminal
			data-socket=(t.socket) data-xterm=(*XTERM_JS_HREF) data-fit=(*XTERM_FIT_JS_HREF) {
			div class="ui-terminal-bar" {
				p class="ui-terminal-state" data-state="connecting" {
					span class="ui-dot" aria-hidden="true" {}
					span data-label { "Connecting…" }
				}
				div class="ui-terminal-tools" role="toolbar" aria-label="Terminal tools" {
					button type="button" class=(TOOL) data-act="copy" { "Copy" }
					button type="button" class=(TOOL) data-act="paste" { "Paste" }
					button type="button" class=(TOOL) data-act="reconnect" hidden { "Reconnect" }
					button type="button" class=(TOOL) data-act="full" aria-pressed="false" {
						span { "Full screen" }
					}
				}
			}
			div class="ui-terminal-screen" data-screen role="group" aria-label=(t.label) {}
			p class="ui-terminal-note" data-note role="status" {}
			noscript { p class="ui-terminal-hint" { "The terminal needs JavaScript." } }
		}
		p class="ui-terminal-hint" {
			"Closes after 15 minutes with nothing typed or printed. Ctrl+Shift+C copies the selection and Ctrl+Shift+V pastes."
			@if t.recorded {
				" This team records what its terminals print, so people with access to the audit log can read it back."
			}
		}
		(terminal_assets())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_frame_names_its_socket_and_loads_only_its_own_assets() {
		let html = terminal(&Terminal {
			label: "Terminal in <b>web</b>",
			socket: "/ws/terminal/app-1?team=2&project=3",
			recorded: true,
		})
		.into_string();
		assert!(html.contains("data-socket=\"/ws/terminal/app-1?team=2&amp;project=3\""));
		assert!(html.contains("/static/xterm.js?v="));
		assert!(html.contains("/static/terminal.js?v="));
		assert!(html.contains("/static/xterm.css?v="));
		assert!(html.contains("records what its terminals print"));
		// A resource's name is data: it is escaped, never markup.
		assert!(!html.contains("<b>web</b>"), "{html}");
		assert!(html.contains("aria-label=\"Terminal in &lt;b&gt;web&lt;/b&gt;\""));
		assert!(!html.contains("<h1"), "the page has its own h1");
	}

	#[test]
	fn an_unrecorded_team_is_not_told_it_is_recorded() {
		let html = terminal(&Terminal {
			label: "Terminal",
			socket: "/ws/terminal/app-1",
			recorded: false,
		})
		.into_string();
		assert!(!html.contains("records what"));
	}
}
