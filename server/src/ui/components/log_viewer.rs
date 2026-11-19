//! Log viewer: a scrollable, monospace region that fills from an SSE stream (`live::job_log`
//! or `live::container_log` append lines into `#<id>`), with a live/ended indicator, level
//! filter chips, a follow toggle, a search box and a copy button.
//!
//! - The region is focusable and named, so the keyboard scrolls it (arrows, PgUp/PgDn,
//!   Home/End) once Tab has reached it.
//! - Every line carries `data-lv` (`INFO`, `ERR`, `DEBUG`, ...), which colors it and which
//!   the filter chips (All, Info, Warn, Error) key on: the chip sets `data-filter` on the
//!   region and `components.css` hides the other levels. No round trip.
//! - Search hides the lines that do not match; lines that arrive later are filtered as they
//!   land.
//! - Copy takes the visible lines. The Clipboard API is withheld over plain HTTP, so the
//!   fallback copies through a selected text area.
//! - Follow keeps the newest line in view; turn it off to read.
//!
//! The page holds one viewer: it owns the signals `_live`, `_follow`, `_q` and `_copied`.

use maud::{Markup, html};

use super::icon::icon;
use crate::ui::sse::js_string;

const SEARCH: &str = "ui-field";
const TOOL: &str = "ui-button ui-button-outline ui-button-sm";

/// The filters: the chip's word and the level it keeps (`None`: all lines).
const FILTERS: [(&str, Option<&str>); 4] = [
	("All", None),
	("Info", Some("INFO")),
	("Warn", Some("WARN")),
	("Error", Some("ERR")),
];

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

/// Sets (or clears) the level filter on the region and moves `aria-pressed` to this chip.
fn filter_script(scroll_id: &str, level: Option<&str>) -> String {
	let set = level.map_or_else(
		|| "delete s.dataset.filter".to_owned(),
		|l| format!("s.dataset.filter = {}", js_string(l)),
	);
	format!(
		"const s = document.getElementById({}); {set}; for (const b of el.parentElement.children) b.setAttribute('aria-pressed', String(b === el))",
		js_string(scroll_id)
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
		div class="flex flex-col gap-3" data-signals="{_live: true, _follow: true, _q: '', _copied: false}" {
			div class="flex flex-wrap items-center gap-3" {
				span class="inline-flex items-center gap-3 text-xs text-muted-foreground" role="status" {
					span class="ui-dot ui-dot-progress" "data-class:ui-dot-progress"="$_live" "data-class:ui-dot-neutral"="!$_live" aria-hidden="true" {}
					span "data-text"="$_live ? 'Live' : 'Ended'" { "Live" }
				}
				div class="flex flex-wrap items-center gap-2" role="group" aria-label="Show log levels" {
					@for (word, level) in FILTERS {
						button type="button" class="ui-chip" aria-pressed=(if level.is_none() { "true" } else { "false" })
							"data-on:click"=(filter_script(&scroll_id, level)) { (word) }
					}
				}
				div class="ui-field-icon ml-auto min-w-40 flex-1 sm:max-w-64 sm:flex-none" {
					(icon("search"))
					input type="search" class=(SEARCH) autocomplete="off" placeholder="Search the log"
						aria-label={ "Search the " (label.to_lowercase()) }
						"data-bind:_q" "data-on:input__debounce.150ms"=(search);
				}
				label class="ui-check text-xs text-muted-foreground" {
					input type="checkbox" class="ui-checkbox" "data-bind:_follow";
					"Follow"
				}
				button type="button" class=(TOOL) "data-on:click"=(copy_script(id)) {
					(icon("copy"))
					span "data-text"="$_copied ? 'Copied' : 'Copy'" { "Copy" }
				}
			}
			div id=(scroll_id) class="ui-log" tabindex="0" role="region" aria-label=(label) "data-init"=(apply_script()) {
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
	fn the_level_chips_are_toggle_buttons_and_only_all_starts_pressed() {
		let html = log_viewer("l", "Log", "/s").into_string();
		assert_eq!(html.matches("aria-pressed=\"true\"").count(), 1, "{html}");
		assert_eq!(html.matches("aria-pressed=\"false\"").count(), 3, "{html}");
		assert!(
			html.contains("role=\"group\" aria-label=\"Show log levels\""),
			"{html}"
		);
		for level in ["INFO", "WARN", "ERR"] {
			assert!(
				html.contains(&format!("s.dataset.filter = &#39;{level}&#39;"))
					|| html.contains(&format!("s.dataset.filter = '{level}'")),
				"{level}: {html}"
			);
		}
		assert!(html.contains("delete s.dataset.filter"), "{html}");
	}

	#[test]
	fn ids_and_labels_cannot_break_out_of_the_script_or_markup() {
		let html = log_viewer("a');alert(1);//", "<b>x</b>", "/s').x").into_string();
		assert!(!html.contains("<b>"), "{html}");
		assert!(!html.contains("getElementById('a');alert"), "{html}");
		assert!(!html.contains("@get('/s').x"), "{html}");
	}
}
