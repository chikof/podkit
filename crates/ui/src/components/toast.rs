//! Toast: a short message about the result of an action. The region exists on every page
//! (`document` renders it) so assistive technology finds it in the same place; a toast
//! is appended to it (Datastar `patch_into("#toasts", Append, ...)`, see `sse::toast`).
//!
//! A toast never carries the only copy of an important result: show errors inline too.
//! `Danger` toasts are announced at once (`role="alert"`), the rest politely, through the
//! region's `aria-live`. Every toast has a Close button.

use maud::{Markup, html};

use super::badge::Tone;
use super::icon::icon_with;

/// The live region all toasts go into. Present on every page, empty until one arrives.
pub fn toast_region() -> Markup {
	toast_region_with(&html! {})
}

/// The region with toasts already in it: the message an action left for this page (see
/// `ui::shell::flash`).
pub fn toast_region_with(initial: &Markup) -> Markup {
	html! {
		section id="toasts" aria-label="Notifications" aria-live="polite" class="pointer-events-none fixed inset-x-4 bottom-4 z-50 ml-auto flex max-w-sm flex-col gap-2" { (initial) }
	}
}

const fn tone_icon(tone: Tone) -> (&'static str, &'static str) {
	match tone {
		Tone::Neutral => ("info", "text-muted-foreground"),
		Tone::Progress => ("loader-circle", "text-info"),
		Tone::Success => ("circle-check", "text-success"),
		Tone::Warning => ("triangle-alert", "text-warning"),
		Tone::Danger => ("circle-x", "text-destructive"),
	}
}

pub fn toast(tone: Tone, title: &str, description: Option<&str>) -> Markup {
	let (name, color) = tone_icon(tone);
	let role = (tone == Tone::Danger).then_some("alert");
	html! {
		div class="ui-toast" data-toast role=[role] {
			(icon_with(name, color))
			div class="ui-toast-body" {
				p class="ui-toast-title" { (title) }
				@if let Some(description) = description {
					p class="ui-toast-text" { (description) }
				}
			}
			button type="button" aria-label="Dismiss" class="ui-toast-x" "data-on:click"="el.closest('[data-toast]').remove()" {
				(icon_with("x", ""))
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_region_is_a_named_polite_live_region() {
		let html = toast_region().into_string();
		assert!(html.contains("id=\"toasts\""), "{html}");
		assert!(html.contains("aria-live=\"polite\""), "{html}");
		assert!(html.contains("aria-label=\"Notifications\""), "{html}");
	}

	#[test]
	fn only_a_danger_toast_interrupts() {
		assert!(
			toast(Tone::Danger, "Failed", None)
				.into_string()
				.contains("role=\"alert\"")
		);
		for tone in [Tone::Neutral, Tone::Progress, Tone::Success, Tone::Warning] {
			assert!(!toast(tone, "Ok", None).into_string().contains("role="));
		}
	}

	#[test]
	fn a_toast_can_be_dismissed_and_escapes_its_text() {
		let html = toast(Tone::Success, "<b>Saved</b>", Some("a & b")).into_string();
		assert!(html.contains("aria-label=\"Dismiss\""), "{html}");
		assert!(
			html.contains("el.closest('[data-toast]').remove()"),
			"{html}"
		);
		assert!(html.contains("&lt;b&gt;Saved&lt;/b&gt;"), "{html}");
		assert!(html.contains("a &amp; b"), "{html}");
	}
}
