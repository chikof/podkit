//! The Monitoring tab's charts beyond CPU and memory: HTTP for apps and stacks, connections and
//! size for databases, tokens for models, plus the deploy and restart markers every chart
//! carries and the counters above them.
//!
//! A page calls [`resource_monitoring`] where it used to call `cpu_and_memory`, with the
//! rows it got from `metrics::service::extras` (never the database):
//!
//! ```ignore
//! let extras = metrics_service::extras(&s.db, user, team, Target::Application(p, a), range).await?;
//! metrics_section(&section, &resource_monitoring(Flavor::Application, &points, &extras, range, now))
//! ```
//!
//! and serves the 30-second refresh with `sse::patch` of the same call. Every chart is the same
//! `line_chart_with` as the rest (so the same tooltip, keyboard reading and data tables), and a
//! group that has no reading yet is one sentence instead of a row of empty charts.

use maud::{Markup, html};

use super::metrics::{cpu_and_memory, metrics_body, range_phrase};
use crate::charts::{Chart, Marker, MarkerKind, Series, Unit, line_chart_with};
use podkit_core::metrics::events::{DeployMarker, EventMarker};
use podkit_core::metrics::ext::ExtPoint;
use podkit_core::metrics::service::Extras;
use podkit_core::metrics::{Range, ResourcePoint};

/// What kind of resource a Monitoring tab is for, which decides the extra charts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
	/// HTTP through the proxy, and the volumes.
	Application,
	/// The same, summed over the services.
	Stack,
	/// Connections, data size and activity. `ops` is what the engine counts: transactions for
	/// `PostgreSQL`, commands for the others.
	Database { ops: Ops },
	/// Tokens and requests the gateway served, and GPU memory.
	Model,
}

/// What a database engine's activity counter is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ops {
	Transactions,
	Commands,
}

/// `Deploy abcdef1 · running`, `Crash: exit code 1`, `Out-of-memory kill`, `Restarted`.
fn deploy_label(d: &DeployMarker) -> String {
	let sha = d
		.commit
		.as_deref()
		.map(|c| c.chars().take(7).collect::<String>());
	match sha {
		Some(sha) if !sha.is_empty() => format!("Deploy {sha} · {}", d.status),
		_ => format!("Deploy · {}", d.status),
	}
}

fn event_label(e: &EventMarker) -> String {
	let what = match (e.event.as_str(), e.exit_code) {
		("exit", Some(code)) => format!("Crash: exit code {code}"),
		("exit", None) => "Crash".to_owned(),
		("oom", _) => "Out-of-memory kill".to_owned(),
		_ => "Restarted".to_owned(),
	};
	if e.container.is_empty() {
		what
	} else {
		format!("{what} · {}", e.container)
	}
}

/// The markers of a response, oldest first.
#[must_use]
pub fn markers(extras: &Extras) -> Vec<Marker> {
	let mut out: Vec<Marker> = extras
		.deploys
		.iter()
		.map(|d| Marker {
			at: d.at,
			kind: MarkerKind::Deploy,
			label: deploy_label(d),
		})
		.chain(extras.events.iter().map(|e| Marker {
			at: e.at,
			kind: MarkerKind::Event,
			label: event_label(e),
		}))
		.collect();
	out.sort_by_key(|m| m.at);
	out
}

/// Crashes, OOM kills and restarts in the window, with the last exit code: the numbers behind
/// the red lines.
fn counters(extras: &Extras, range: Range) -> Markup {
	let count = |event: &str| extras.events.iter().filter(|e| e.event == event).count();
	let last_exit = extras
		.events
		.iter()
		.rev()
		.find_map(|e| (e.event == "exit").then_some(e.exit_code).flatten());
	html! {
		div class="col-span-full" {
			h3 class="ui-visually-hidden" { "Container events, " (range_phrase(range)) }
			dl class="ui-counters" {
				div { dt { "Crashes" } dd { (count("exit")) } }
				div { dt { "Out-of-memory kills" } dd { (count("oom")) } }
				div { dt { "Restarts" } dd { (count("restart")) } }
				@if let Some(code) = last_exit { div { dt { "Last exit code" } dd { (code) } } }
			}
		}
	}
}

fn note(text: &str) -> Markup {
	html! { p class="ui-chart-empty col-span-full" { (text) } }
}

/// A series: its label, chart colour and how to read it from a point.
type Lines<'a> = (&'a str, u8, fn(&ExtPoint) -> Option<f64>);

struct Frame<'a> {
	xs: Vec<i64>,
	points: &'a [ExtPoint],
	range: Range,
	from: i64,
	now: i64,
	markers: &'a [Marker],
}

impl<'a> Frame<'a> {
	fn new(points: &'a [ExtPoint], range: Range, now: i64, markers: &'a [Marker]) -> Self {
		Self {
			xs: points.iter().map(|p| p.at).collect(),
			points,
			range,
			from: now - range.duration().whole_seconds(),
			now,
			markers,
		}
	}

	fn has(&self, get: fn(&ExtPoint) -> Option<f64>) -> bool {
		self.points.iter().any(|p| get(p).is_some())
	}

	fn chart(&self, id: &str, title: &str, unit: Unit, series: Vec<Lines<'_>>) -> Markup {
		let chart = Chart {
			id,
			title,
			range: range_phrase(self.range),
			unit,
			max: None,
			from: self.from,
			to: self.now,
			xs: &self.xs,
			series: series
				.into_iter()
				.map(|(label, color, get)| Series {
					label,
					color,
					values: self.points.iter().map(get).collect(),
				})
				.collect(),
		};
		line_chart_with(&chart, self.markers)
	}
}

fn int(v: Option<i64>) -> Option<f64> {
	#[allow(clippy::cast_precision_loss, reason = "a chart value")]
	v.map(|v| v as f64)
}

fn http_charts(f: &Frame<'_>) -> Markup {
	if !f.has(|p| p.req_rate) {
		return note(
			"No HTTP traffic recorded in this range. Requests through the proxy show up here once there is a domain and someone visits it.",
		);
	}
	html! {
		(f.chart("http-requests", "Requests", Unit::Rate("req/s"), vec![
			("2xx", 2, |p| p.r2xx_rate),
			("3xx", 1, |p| p.r3xx_rate),
			("4xx", 4, |p| p.r4xx_rate),
			("5xx", 5, |p| p.r5xx_rate),
		]))
		(f.chart("http-latency", "Response time", Unit::Millis, vec![
			("p50", 2, |p| p.p50_ms),
			("p95", 4, |p| p.p95_ms),
		]))
		(f.chart("http-traffic", "Traffic", Unit::BytesPerSec, vec![
			("Received", 1, |p| p.bytes_in_rate),
			("Sent", 5, |p| p.bytes_out_rate),
		]))
	}
}

fn volume_chart(f: &Frame<'_>) -> Markup {
	html! {
		@if f.points.iter().any(|p| p.volume_bytes.is_some()) {
			(f.chart("volume", "Volume use", Unit::Bytes, vec![("Used", 3, |p| int(p.volume_bytes))]))
		}
	}
}

fn database_charts(f: &Frame<'_>, ops: Ops) -> Markup {
	if !f
		.points
		.iter()
		.any(|p| p.db_conns.is_some() || p.db_size.is_some())
	{
		return note(
			"No readings from the database engine in this range yet. podkit asks the running database every 30 seconds.",
		);
	}
	let (title, unit, label) = match ops {
		Ops::Transactions => ("Transactions", "tx/s", "Transactions"),
		Ops::Commands => ("Commands", "cmd/s", "Commands"),
	};
	html! {
		(f.chart("db-connections", "Connections", Unit::Count, vec![("Open", 2, |p| int(p.db_conns))]))
		(f.chart("db-size", "Data size", Unit::Bytes, vec![("Size", 4, |p| int(p.db_size))]))
		@if f.has(|p| p.db_ops_rate) {
			(f.chart("db-ops", title, Unit::Rate(unit), vec![(label, 1, |p| p.db_ops_rate)]))
		}
	}
}

fn model_charts(f: &Frame<'_>) -> Markup {
	html! {
		@if f.has(|p| p.tokens_rate) {
			(f.chart("tokens", "Tokens", Unit::Rate("tokens/s"), vec![("Generated and read", 2, |p| p.tokens_rate)]))
			(f.chart("model-requests", "Requests", Unit::Rate("req/s"), vec![("Served", 1, |p| p.model_req_rate)]))
		} @else {
			(note("No tokens served in this range. They show up here once an app calls this model through the gateway."))
		}
		@if f.points.iter().any(|p| p.vram_bytes.is_some()) {
			(f.chart("vram", "GPU memory", Unit::Bytes, vec![("VRAM", 5, |p| int(p.vram_bytes))]))
		}
	}
}

/// The whole body of a Monitoring tab: the event counters, CPU and memory, then the charts of
/// the resource's flavor. Every chart carries the deploy and restart markers.
#[must_use]
pub fn resource_monitoring(
	flavor: Flavor,
	points: &[ResourcePoint],
	extras: &Extras,
	range: Range,
	now: i64,
) -> Markup {
	let marks = markers(extras);
	let f = Frame::new(&extras.points, range, now, &marks);
	metrics_body(
		&html! {
			(counters(extras, range))
			(cpu_and_memory(points, range, now, &marks))
			@match flavor {
				Flavor::Application | Flavor::Stack => { (http_charts(&f)) (volume_chart(&f)) }
				Flavor::Database { ops } => { (database_charts(&f, ops)) (volume_chart(&f)) }
				Flavor::Model => { (model_charts(&f)) }
			}
		},
		now,
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	const NOW: i64 = 1_700_000_000;

	fn base() -> Vec<ResourcePoint> {
		(0..5)
			.map(|i| ResourcePoint {
				at: NOW - 3000 + i * 600,
				cpu: Some(5.0),
				mem: 64 << 20,
			})
			.collect()
	}

	fn http_points() -> Vec<ExtPoint> {
		(0..5)
			.map(|i| ExtPoint {
				at: NOW - 3000 + i * 600,
				req_rate: Some(10.0),
				r2xx_rate: Some(8.0),
				r3xx_rate: Some(0.0),
				r4xx_rate: Some(1.5),
				r5xx_rate: Some(0.5),
				p50_ms: Some(12.0),
				p95_ms: Some(1500.0),
				bytes_in_rate: Some(1024.0),
				bytes_out_rate: Some(4096.0),
				volume_bytes: Some(3 << 30),
				..ExtPoint::default()
			})
			.collect()
	}

	fn deploy(at: i64, status: &str, commit: Option<&str>) -> DeployMarker {
		DeployMarker {
			at,
			id: 1,
			status: status.to_owned(),
			commit: commit.map(str::to_owned),
		}
	}

	fn event(at: i64, kind: &str, code: Option<i32>) -> EventMarker {
		EventMarker {
			at,
			event: kind.to_owned(),
			exit_code: code,
			container: "podkit-app-7-3".to_owned(),
		}
	}

	#[test]
	fn deploy_markers_render_on_every_chart_with_their_words() {
		let extras = Extras {
			points: http_points(),
			deploys: vec![deploy(NOW - 1800, "running", Some("abcdef1234567890"))],
			events: vec![event(NOW - 600, "oom", None)],
		};
		let html = resource_monitoring(Flavor::Application, &base(), &extras, Range::Hour, NOW)
			.into_string();
		// CPU, memory, three HTTP charts and the volume: seven figures, each with both markers.
		assert_eq!(html.matches("<figure").count(), 6, "{html}");
		assert_eq!(
			html.matches("data-m=\"deploy\"").count(),
			6 * 2,
			"line and key: {html}"
		);
		assert_eq!(html.matches("data-m=\"event\"").count(), 6 * 2, "{html}");
		// The tooltip words ride in `data-points`, escaped as an attribute.
		assert!(html.contains("Deploy abcdef1 · running"), "{html}");
		assert!(
			html.contains("Out-of-memory kill · podkit-app-7-3"),
			"{html}"
		);
		// The deploy sits halfway through the hour-long window: `--x:` is a percentage.
		assert!(html.contains("--x:50"), "{html}");
	}

	#[test]
	fn marker_words_are_escaped() {
		let extras = Extras {
			points: Vec::new(),
			deploys: vec![deploy(NOW - 60, "<script>alert(1)</script>", None)],
			events: Vec::new(),
		};
		let html = resource_monitoring(Flavor::Application, &base(), &extras, Range::Hour, NOW)
			.into_string();
		assert!(!html.contains("<script>alert"), "{html}");
	}

	#[test]
	fn the_counters_count_crashes_oom_kills_and_restarts() {
		let extras = Extras {
			events: vec![
				event(NOW - 900, "exit", Some(1)),
				event(NOW - 800, "exit", Some(137)),
				event(NOW - 700, "oom", None),
				event(NOW - 600, "restart", None),
			],
			..Extras::default()
		};
		let html = counters(&extras, Range::Hour).into_string();
		assert!(
			html.contains("<dt>Crashes</dt> <dd>2</dd>")
				|| html.contains("<dt>Crashes</dt><dd>2</dd>"),
			"{html}"
		);
		assert!(html.contains("Last exit code"), "{html}");
		assert!(html.contains("<dd>137</dd>"), "{html}");
		let quiet = counters(&Extras::default(), Range::Day).into_string();
		assert!(!quiet.contains("Last exit code"), "{quiet}");
		assert!(quiet.contains("last 24 hours"), "{quiet}");
	}

	#[test]
	fn an_app_without_traffic_gets_one_sentence_not_empty_charts() {
		let html = resource_monitoring(
			Flavor::Application,
			&base(),
			&Extras::default(),
			Range::Hour,
			NOW,
		)
		.into_string();
		assert!(html.contains("No HTTP traffic recorded"), "{html}");
		assert!(!html.contains("id=\"chart-http-requests\""), "{html}");
		assert!(html.contains("id=\"chart-cpu\""), "{html}");
	}

	#[test]
	fn a_database_shows_connections_size_and_its_own_word_for_activity() {
		let points: Vec<ExtPoint> = (0..4)
			.map(|i| ExtPoint {
				at: NOW - 2400 + i * 600,
				db_conns: Some(4),
				db_size: Some(50 << 20),
				db_ops_rate: Some(12.5),
				..ExtPoint::default()
			})
			.collect();
		let extras = Extras {
			points,
			..Extras::default()
		};
		let pg = resource_monitoring(
			Flavor::Database {
				ops: Ops::Transactions,
			},
			&base(),
			&extras,
			Range::Hour,
			NOW,
		)
		.into_string();
		for id in ["chart-db-connections", "chart-db-size", "chart-db-ops"] {
			assert!(pg.contains(id), "{id}: {pg}");
		}
		assert!(pg.contains("12.5 tx/s"), "{pg}");
		let redis = resource_monitoring(
			Flavor::Database { ops: Ops::Commands },
			&base(),
			&extras,
			Range::Hour,
			NOW,
		)
		.into_string();
		assert!(redis.contains("12.5 cmd/s"), "{redis}");
	}

	#[test]
	fn a_model_shows_tokens_and_gpu_memory() {
		let points: Vec<ExtPoint> = (0..3)
			.map(|i| ExtPoint {
				at: NOW - 1800 + i * 600,
				tokens_rate: Some(42.0),
				model_req_rate: Some(0.5),
				vram_bytes: Some(6 << 30),
				..ExtPoint::default()
			})
			.collect();
		let extras = Extras {
			points,
			..Extras::default()
		};
		let html =
			resource_monitoring(Flavor::Model, &base(), &extras, Range::Hour, NOW).into_string();
		assert!(html.contains("42 tokens/s"), "{html}");
		assert!(html.contains("id=\"chart-vram\""), "{html}");
		assert!(html.contains("6 GB"), "{html}");
	}
}
