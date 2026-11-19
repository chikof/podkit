//! Banner: a block-level message with an icon, a lead line, a sentence and an optional
//! action ("Retry"). The kind shows in the icon's shape as well as its color, so it never
//! depends on color alone, and an error is announced at once (`role="alert"`) while the
//! others wait their turn (`role="status"`).
//!
//! Use it for something about the whole page or form (a failed load, a failed service). A
//! message about one field is `error_text`; the result of an action is a toast.

use maud::{Markup, html};

use super::icon::icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
	Error,
	Warning,
	Info,
	Success,
}

impl BannerKind {
	const fn class(self) -> &'static str {
		match self {
			Self::Error => "ui-banner",
			Self::Warning => "ui-banner ui-banner-warn",
			Self::Info => "ui-banner ui-banner-info",
			Self::Success => "ui-banner ui-banner-ok",
		}
	}

	const fn icon(self) -> &'static str {
		match self {
			Self::Error => "circle-alert",
			Self::Warning => "triangle-alert",
			Self::Info => "info",
			Self::Success => "circle-check",
		}
	}

	const fn role(self) -> &'static str {
		match self {
			Self::Error => "alert",
			_ => "status",
		}
	}
}

/// `lead` is the bold first sentence, `text` the rest; `action` is a button or link.
pub fn banner(kind: BannerKind, lead: Option<&str>, text: &str, action: Option<&Markup>) -> Markup {
	html! {
		div class=(kind.class()) role=(kind.role()) {
			(icon(kind.icon()))
			div class="ui-banner-body" {
				@if let Some(lead) = lead { b { (lead) } " " }
				(text)
			}
			@if let Some(action) = action { (action) }
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_error_interrupts_and_the_others_wait() {
		assert!(
			banner(BannerKind::Error, None, "x", None)
				.into_string()
				.contains("role=\"alert\"")
		);
		for kind in [BannerKind::Warning, BannerKind::Info, BannerKind::Success] {
			let html = banner(kind, None, "x", None).into_string();
			assert!(html.contains("role=\"status\""), "{html}");
			assert!(!html.contains("role=\"alert\""), "{html}");
		}
	}

	#[test]
	fn each_kind_has_its_own_class_and_icon_shape() {
		let kinds = [
			BannerKind::Error,
			BannerKind::Warning,
			BannerKind::Info,
			BannerKind::Success,
		];
		for (i, a) in kinds.iter().enumerate() {
			for b in &kinds[i + 1..] {
				assert_ne!(a.class(), b.class());
				assert_ne!(a.icon(), b.icon());
			}
		}
	}

	#[test]
	fn text_is_escaped_and_the_action_is_optional() {
		let html = banner(
			BannerKind::Error,
			Some("<b>Lead</b>"),
			"a & <i>b</i>",
			Some(&html! { button type="button" { "Retry" } }),
		)
		.into_string();
		assert!(!html.contains("<i>") && !html.contains("<b>Lead"), "{html}");
		assert!(html.contains("a &amp; &lt;i&gt;b&lt;/i&gt;"), "{html}");
		assert!(html.contains("Retry"), "{html}");
		assert!(
			!banner(BannerKind::Info, None, "x", None)
				.into_string()
				.contains("button")
		);
	}
}
