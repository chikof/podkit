//! Responses for Datastar actions: one SSE stream of patch events. Helpers
//! return [`Events`] so a handler can chain several with [`Events::and`].

use std::convert::Infallible;
use std::fmt::Write as _;

use axum::response::{IntoResponse, Response, Sse, sse::Event};
use datastar::consts::ElementPatchMode;
use datastar::prelude::*;
use maud::{Markup, html};

pub struct Events(Vec<Event>);

impl Events {
	#[must_use]
	pub fn and(mut self, other: Self) -> Self {
		self.0.extend(other.0);
		self
	}
}

impl IntoResponse for Events {
	fn into_response(self) -> Response {
		Sse::new(futures_util::stream::iter(
			self.0.into_iter().map(Ok::<_, Infallible>),
		))
		.into_response()
	}
}

/// A JS single-quoted string literal. Anything outside a small safe set is
/// written as a `\u` escape, so no input can leave the literal.
pub(super) fn js_string(s: &str) -> String {
	let mut out = String::with_capacity(s.len() + 2);
	out.push('\'');
	for c in s.chars() {
		if c.is_ascii_alphanumeric() || "/-_.~?=&%:#@!$()*+,;".contains(c) {
			out.push(c);
		} else {
			let mut buf = [0u16; 2];
			for unit in c.encode_utf16(&mut buf) {
				let _ = write!(out, "\\u{unit:04x}");
			}
		}
	}
	out.push('\'');
	out
}

/// Navigates the browser to `url`.
pub fn redirect(url: &str) -> Events {
	let script = format!("window.location.assign({})", js_string(url));
	Events(vec![ExecuteScript::new(script).write_as_axum_sse_event()])
}

/// Sets signals, e.g. to clear a form after success.
pub fn signals(json: &serde_json::Value) -> Events {
	Events(vec![
		PatchSignals::new(json.to_string()).write_as_axum_sse_event(),
	])
}

/// Shows `msg` in the `_error` signal (rendered by `components::form_error`).
pub fn error(msg: &str) -> Events {
	signals(&serde_json::json!({ "_error": msg }))
}

/// Morphs `markup` into the page; the root element's `id` picks the target.
pub fn patch(markup: &Markup) -> Events {
	Events(vec![
		PatchElements::new(markup.0.clone()).write_as_axum_sse_event(),
	])
}

/// Patches `markup` into the element matching `selector` using `mode`.
pub fn patch_into(selector: &str, mode: ElementPatchMode, markup: &Markup) -> Events {
	Events(vec![
		PatchElements::new(markup.0.clone())
			.selector(selector)
			.mode(mode)
			.write_as_axum_sse_event(),
	])
}

/// A short message in the corner that removes itself after a few seconds.
pub fn toast(msg: &str) -> Events {
	let markup = html! {
		div class="pointer-events-auto rounded-lg border border-border bg-card px-4 py-3 text-sm text-card-foreground" role="status" "data-init"="setTimeout(() => el.remove(), 4000)" { (msg) }
	};
	patch_into("#toasts", ElementPatchMode::Append, &markup)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn js_string_cannot_leave_its_quotes() {
		assert_eq!(js_string("/teams/1"), "'/teams/1'");
		let nasty = js_string("/');alert(1);//\n</script>");
		assert!(!nasty[1..nasty.len() - 1].contains('\''), "{nasty}");
		assert!(!nasty.contains('<') && !nasty.contains('\n'), "{nasty}");
	}
}
