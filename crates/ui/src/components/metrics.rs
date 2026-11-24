//! Metrics section: the frame every resource page (server, application, database, model,
//! stack) puts its charts in, plus the CPU and memory charts the four resource kinds share.
//!
//! A page embeds it like this:
//!
//! ```ignore
//! let section = MetricsSection { id: "app", page_url, refresh_url, range };
//! metrics_section(&section, &resource_monitoring(flavor, &points, &extras, range, now))
//! ```
//!
//! and serves `refresh_url?range=…` with `sse::patch(&resource_monitoring(…))` (see `metrics_extra`), calling the
//! `metrics::service` fn for its kind (never the database).
//!
//! The frame is stable and the charts inside `#metrics-body` are what gets patched:
//!
//! - **Range**: three links (`1h`, `24h`, `7d`), each a page load with `?range=`.
//! - **Refresh**: Datastar fetches `refresh_url` every 30 seconds, but only
//!   `while document.hidden` is false, and once more as soon as the tab is visible again; a
//!   hidden tab makes no requests.
//! - **Data tables**: the `_tables` signal (client only) shows the table under each chart.

use maud::{Markup, html};

use super::button::{Size, Variant, button_class};
use crate::charts::{Chart, Marker, Series, Unit, line_chart_with};
use crate::sse::js_string;
use podkit_core::metrics::{Range, ResourcePoint};

/// How often a visible page refreshes its charts.
pub const REFRESH_SECONDS: u32 = 30;

/// Where a metrics section lives and where its refresh comes from.
#[derive(Debug, Clone, Copy)]
pub struct MetricsSection<'a> {
	/// The section's id and the prefix of its elements.
	pub id: &'a str,
	/// The page itself, without `range`; links add `?range=` or `&range=`.
	pub page_url: &'a str,
	/// The Datastar endpoint that answers with a patch of `metrics_body`.
	pub refresh_url: &'a str,
	pub range: Range,
}

/// `1h`, `24h` or `7d`: the value of the `range` parameter.
#[must_use]
pub const fn range_value(range: Range) -> &'static str {
	match range {
		Range::Hour => "1h",
		Range::Day => "24h",
		Range::Week => "7d",
	}
}

/// "last hour", for sentences.
#[must_use]
pub const fn range_phrase(range: Range) -> &'static str {
	match range {
		Range::Hour => "last hour",
		Range::Day => "last 24 hours",
		Range::Week => "last 7 days",
	}
}

const RANGES: [Range; 3] = [Range::Hour, Range::Day, Range::Week];

fn with_range(url: &str, range: Range) -> String {
	let sep = if url.contains('?') { '&' } else { '?' };
	format!("{url}{sep}range={}", range_value(range))
}

/// The charts' container. Patching it replaces the charts and nothing else.
#[must_use]
pub fn metrics_body(content: &Markup, now: i64) -> Markup {
	let updated = time::OffsetDateTime::from_unix_timestamp(now).map_or(String::new(), |d| {
		format!("{:02}:{:02}:{:02} UTC", d.hour(), d.minute(), d.second())
	});
	html! {
		div id="metrics-body" class="flex flex-col gap-4" {
			div class="grid gap-x-8 gap-y-8 lg:grid-cols-2" "data-class"="{'lg:grid-cols-2': !$_tables}" { (content) }
			p class="text-xs text-muted-foreground" {
				"Updated " (updated) ". Refreshes every " (REFRESH_SECONDS) " seconds while this tab is visible."
			}
		}
	}
}

/// The section: heading, range links, the data-table toggle and `body` (a `metrics_body`).
#[must_use]
pub fn metrics_section(section: &MetricsSection<'_>, body: &Markup) -> Markup {
	let refresh = js_string(&with_range(section.refresh_url, section.range));
	// `document.hidden ||` is the whole pause: a hidden tab never reaches `@get`.
	let on_interval = format!("document.hidden || @get({refresh})");
	let title_id = format!("{}-title", section.id);
	html! {
		section id=(section.id) aria-labelledby=(title_id) class="flex flex-col gap-4"
			"data-signals"="{_tables: false}"
			"data-on-interval__duration.30s"=(on_interval)
			"data-on:visibilitychange__window"=(on_interval) {
			div class="ui-range-bar" {
				h2 id=(title_id) class="ui-sec-h" { "Metrics" }
				div class="ui-range-tools" {
					nav aria-label="Time range" class="ui-range" {
						@for range in RANGES {
							@if range == section.range {
								a href=(with_range(section.page_url, range)) class="ui-range-link" aria-current="true" { (range_value(range)) }
							} @else {
								a href=(with_range(section.page_url, range)) class="ui-range-link" { (range_value(range)) }
							}
						}
					}
					button type="button" class=(button_class(Variant::Outline, Size::Sm)) aria-pressed="false"
						"data-on:click"="$_tables = !$_tables"
						"data-attr:aria-pressed"="$_tables ? 'true' : 'false'" {
						"Data tables"
					}
				}
			}
			(body)
		}
	}
}

/// The CPU and memory charts of one resource, with `markers` (deployments, restarts) drawn on
/// both: the first two charts of every resource's Monitoring tab. `now` is the end of the
/// window, in unix seconds.
#[must_use]
pub fn cpu_and_memory(
	points: &[ResourcePoint],
	range: Range,
	now: i64,
	markers: &[Marker],
) -> Markup {
	let from = now - range.duration().whole_seconds();
	let xs: Vec<i64> = points.iter().map(|p| p.at).collect();
	let cpu = Chart {
		id: "cpu",
		title: "CPU",
		range: range_phrase(range),
		unit: Unit::Percent,
		// A container may use more than one core, so the axis follows the data.
		max: None,
		from,
		to: now,
		xs: &xs,
		series: vec![Series {
			label: "CPU",
			color: 2,
			values: points.iter().map(|p| p.cpu).collect(),
		}],
	};
	let memory = Chart {
		id: "memory",
		title: "Memory",
		range: range_phrase(range),
		unit: Unit::Bytes,
		max: None,
		from,
		to: now,
		xs: &xs,
		series: vec![Series {
			label: "Memory",
			color: 4,
			values: points.iter().map(|p| Some(p.mem as f64)).collect(),
		}],
	};
	html! {
		(line_chart_with(&cpu, markers))
		(line_chart_with(&memory, markers))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// What the Monitoring tabs showed before the extra charts: CPU and memory, no markers.
	fn resource_charts(points: &[ResourcePoint], range: Range, now: i64) -> Markup {
		metrics_body(&cpu_and_memory(points, range, now, &[]), now)
	}

	const NOW: i64 = 1_700_000_000;

	fn section(range: Range) -> MetricsSection<'static> {
		MetricsSection {
			id: "metrics",
			page_url: "/teams/1/projects/2/applications/3",
			refresh_url: "/teams/1/projects/2/applications/3/metrics",
			range,
		}
	}

	fn points(n: i64) -> Vec<ResourcePoint> {
		(0..n)
			.map(|i| ResourcePoint {
				at: NOW - 3600 + i * 60,
				cpu: Some(10.0 + i as f64),
				mem: 100 * 1024 * 1024,
			})
			.collect()
	}

	#[test]
	fn the_section_polls_only_while_the_tab_is_visible() {
		let html = metrics_section(
			&section(Range::Hour),
			&resource_charts(&points(3), Range::Hour, NOW),
		)
		.into_string();
		let guard =
			"document.hidden || @get('/teams/1/projects/2/applications/3/metrics?range=1h')";
		assert!(
			html.contains(&format!("data-on-interval__duration.30s=\"{guard}\"")),
			"{html}"
		);
		// Back in the foreground it refreshes at once instead of waiting for the timer.
		assert!(
			html.contains(&format!("data-on:visibilitychange__window=\"{guard}\"")),
			"{html}"
		);
		// The timer is on the stable frame, so a patch of the charts does not restart it.
		let (frame, body) = html.split_once("id=\"metrics-body\"").unwrap();
		assert!(frame.contains("data-on-interval"), "{html}");
		assert!(!body.contains("data-on-interval"), "{html}");
	}

	#[test]
	fn the_range_links_mark_the_current_range_and_keep_other_query_parameters() {
		let html = metrics_section(&section(Range::Day), &html! {}).into_string();
		assert_eq!(html.matches("aria-current=\"true\"").count(), 1, "{html}");
		let (_, after) = html.split_once("aria-current=\"true\"").unwrap();
		assert!(after.starts_with(">24h"), "{html}");
		for r in ["?range=1h", "?range=24h", "?range=7d"] {
			assert!(html.contains(r), "{r}: {html}");
		}
		assert_eq!(
			with_range("/x?environment=4", Range::Week),
			"/x?environment=4&range=7d"
		);
	}

	#[test]
	fn the_toggle_shows_the_data_tables_and_says_whether_it_is_on() {
		let html = metrics_section(
			&section(Range::Hour),
			&resource_charts(&points(5), Range::Hour, NOW),
		)
		.into_string();
		assert!(html.contains("data-signals=\"{_tables: false}\""), "{html}");
		assert!(
			html.contains("data-on:click=\"$_tables = !$_tables\""),
			"{html}"
		);
		assert!(html.contains("aria-pressed=\"false\""), "{html}");
		assert!(html.contains("data-attr:aria-pressed"), "{html}");
		// Every chart's table is behind the same signal.
		assert_eq!(html.matches("data-show=\"$_tables\"").count(), 2, "{html}");
		assert!(html.contains("<h2 id=\"metrics-title\""), "{html}");
		// With the tables on, the charts stack in one wide column so a table is not squeezed.
		assert!(
			html.contains("data-class=\"{'lg:grid-cols-2': !$_tables}\""),
			"{html}"
		);
	}

	#[test]
	fn resource_charts_draw_cpu_and_memory_and_stamp_the_refresh() {
		let html = resource_charts(&points(10), Range::Hour, NOW).into_string();
		assert!(
			html.contains("id=\"chart-cpu\"") && html.contains("id=\"chart-memory\""),
			"{html}"
		);
		assert!(html.contains("Latest 19%"), "{html}");
		assert!(html.contains("100 MB"), "{html}");
		assert!(html.contains("Updated 22:13:20 UTC"), "{html}");
		let empty = resource_charts(&[], Range::Week, NOW).into_string();
		assert!(empty.contains("No readings in this range yet"), "{empty}");
	}

	#[test]
	fn the_range_words_match_the_api() {
		for r in RANGES {
			assert_eq!(Range::parse(range_value(r)), Some(r));
		}
		assert_eq!(range_phrase(Range::Week), "last 7 days");
	}
}
