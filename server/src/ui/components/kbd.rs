//! Kbd: a keyboard key or shortcut, for hints (`Esc`, `Ctrl` + `K`). Text only; it does
//! not make the shortcut work.

use maud::{Markup, html};

const KEY: &str = "inline-flex h-5 min-w-5 items-center justify-center rounded-sm border border-border bg-muted px-1 font-mono text-xs font-medium text-muted-foreground";

pub fn kbd(key: &str) -> Markup {
	html! { kbd class=(KEY) { (key) } }
}

/// Keys pressed together, joined with a `+`.
pub fn kbd_combo(keys: &[&str]) -> Markup {
	html! {
		span class="inline-flex items-center gap-1" {
			@for (i, key) in keys.iter().enumerate() {
				@if i > 0 { span class="text-xs text-muted-foreground" aria-hidden="true" { "+" } }
				(kbd(key))
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_key_is_a_kbd_element_with_escaped_text() {
		let html = kbd("<Esc>").into_string();
		assert!(html.starts_with("<kbd "), "{html}");
		assert!(html.contains("&lt;Esc&gt;"), "{html}");
	}

	#[test]
	fn a_combo_joins_keys_with_a_decorative_plus() {
		let html = kbd_combo(&["Ctrl", "K"]).into_string();
		assert_eq!(html.matches("<kbd ").count(), 2, "{html}");
		assert_eq!(html.matches("aria-hidden=\"true\"").count(), 1, "{html}");
	}
}
