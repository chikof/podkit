//! Log viewer: a scrollable, monospace region that fills from an SSE stream (`live::job_log`
//! or `live::container_log` append lines into `#<id>`), with a live/ended indicator, a
//! follow toggle, a search box and a copy button.
//!
//! - The region is focusable and named, so the keyboard scrolls it (arrows, PgUp/PgDn,
//!   Home/End) once Tab has reached it.
//! - Search hides the lines that do not match; it needs no round trip, and lines that
//!   arrive later are filtered as they land.
//! - Copy takes the visible lines. The Clipboard API is withheld over plain HTTP, so the
//!   fallback copies through a selected text area.
//! - Follow keeps the newest line in view; turn it off to read.
//!
//! The page holds one viewer: it owns the signals `_live`, `_follow`, `_q` and `_copied`.

use maud::{Markup, html};

use super::icon::icon;
use crate::ui::sse::js_string;

const SEARCH: &str = "ui-field h-8 w-full min-w-0 rounded-md border border-input bg-transparent pr-3 pl-8 text-sm font-normal shadow-xs transition-colors outline-hidden placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50";
const TOOL: &str = "ui-button inline-flex h-8 shrink-0 cursor-pointer items-center justify-center gap-2 rounded-md border border-input bg-background px-3 text-sm font-medium whitespace-nowrap text-foreground shadow-xs transition-colors outline-hidden select-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring/50";
const SCROLL: &str = "h-96 overflow-auto rounded-lg border border-border bg-card p-3 font-mono text-xs leading-5 text-card-foreground outline-hidden focus-visible:ring-2 focus-visible:ring-ring/50";

/// Hides the lines that miss the search and, with follow on, scrolls to the newest. Stored
/// on the scroller as `_apply` so the search box can call it too.
fn apply_script() -> &'static str {
	"const apply = () => { const q = ($_q || '').toLowerCase(); if (q !== '' || el._filtered) { for (const l of el.firstElementChild.children) l.hidden = q !== '' && !l.textContent.toLowerCase().includes(q); el._filtered = q !== '' } if ($_follow) el.scrollTop = el.scrollHeight }; el._apply = apply; new MutationObserver(apply).observe(el, {childList: true, subtree: true})"
}

fn copy_script(id: &str) -> String {
	format!(
		"const t = document.getElementById({}).innerText; const done = () => {{ $_copied = true; setTimeout(() => $_copied = false, 1600) }}; if (window.isSecureContext && navigator.clipboard) {{ navigator.clipboard.writeText(t).then(done) }} else {{ const a = document.createElement('textarea'); a.value = t; a.setAttribute('readonly', ''); a.style.position = 'fixed'; a.style.opacity = '0'; document.body.append(a); a.select(); document.execCommand('copy'); a.remove(); done() }}",
		js_string(id)
	)
}

/// A viewer named `label` ("Build log") whose lines go into `#id` and come from
/// `stream_url`, which must answer with `job_log` or `container_log` for the same `id`.
pub fn log_viewer(id: &str, label: &str, stream_url: &str) -> Markup {
	let scroll_id = format!("{id}-scroll");
	let search = format!(
		"document.getElementById({})._apply()",
		js_string(&scroll_id)
	);
	let load = format!("@get({})", js_string(stream_url));
	html! {
		div class="flex flex-col gap-2" data-signals="{_live: true, _follow: true, _q: '', _copied: false}" {
			div class="flex flex-wrap items-center gap-2" {
				span class="inline-flex items-center gap-2 text-xs text-muted-foreground" role="status" {
					span class="ui-dot ui-dot-progress" "data-class:ui-dot-progress"="$_live" "data-class:ui-dot-neutral"="!$_live" aria-hidden="true" {}
					span "data-text"="$_live ? 'Live' : 'Ended'" { "Live" }
				}
				div class="relative ml-auto min-w-40 flex-1 sm:max-w-64 sm:flex-none" {
					span class="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-muted-foreground" { (icon("search")) }
					input type="search" class=(SEARCH) autocomplete="off" placeholder="Search the log"
						aria-label={ "Search the " (label.to_lowercase()) }
						"data-bind:_q" "data-on:input__debounce.150ms"=(search);
				}
				label class="flex cursor-pointer items-center gap-2 text-xs text-muted-foreground" {
					input type="checkbox" class="ui-checkbox outline-hidden focus-visible:ring-2 focus-visible:ring-ring/50" "data-bind:_follow";
					"Follow"
				}
				button type="button" class=(TOOL) "data-on:click"=(copy_script(id)) {
					(icon("copy"))
					span "data-text"="$_copied ? 'Copied' : 'Copy'" { "Copy" }
				}
			}
			div id=(scroll_id) class=(SCROLL) tabindex="0" role="region" aria-label=(label) "data-init"=(apply_script()) {
				div id=(id) "data-init"=(load) {}
			}
			p class="sr-only" role="status" "data-text"="$_copied ? 'Copied the log' : ''" {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_log_is_a_named_focusable_region_that_streams_into_its_id() {
		let html = log_viewer("build-log", "Build log", "/d/1/log").into_string();
		assert!(html.contains("id=\"build-log-scroll\""), "{html}");
		assert!(html.contains("tabindex=\"0\""), "{html}");
		assert!(html.contains("role=\"region\""), "{html}");
		assert!(html.contains("aria-label=\"Build log\""), "{html}");
		assert!(html.contains("<div id=\"build-log\""), "{html}");
		assert!(html.contains("@get('/d/1/log')"), "{html}");
	}

	#[test]
	fn search_follow_and_copy_are_reachable_controls() {
		let html = log_viewer("l", "Runtime log", "/s").into_string();
		assert!(html.contains("type=\"search\""), "{html}");
		assert!(
			html.contains("aria-label=\"Search the runtime log\""),
			"{html}"
		);
		assert!(html.contains("data-bind:_follow"), "{html}");
		assert!(html.contains("data-bind:_q"), "{html}");
		assert!(html.contains("navigator.clipboard"), "{html}");
		assert!(html.contains("execCommand('copy')"), "{html}");
		assert!(
			html.contains("document.getElementById('l-scroll')._apply()"),
			"{html}"
		);
	}

	#[test]
	fn ids_and_labels_cannot_break_out_of_the_script_or_markup() {
		let html = log_viewer("a');alert(1);//", "<b>x</b>", "/s').x").into_string();
		assert!(!html.contains("<b>"), "{html}");
		assert!(!html.contains("getElementById('a');alert"), "{html}");
		assert!(!html.contains("@get('/s').x"), "{html}");
	}
}
