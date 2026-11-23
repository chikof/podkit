//! Traefik's Prometheus metrics, turned into per-service request counters.
//!
//! Traefik serves the text exposition format
//! (<https://prometheus.io/docs/instrumenting/exposition_formats/>) on its `metrics`
//! entrypoint, which listens on the container's loopback only and is never published.
//! The agent reads it by running `wget` inside the Traefik container, so no port opens
//! anywhere. The metric names and labels are those of Traefik's reference
//! (<https://doc.traefik.io/traefik/reference/install-configuration/observability/metrics/>):
//! `traefik_service_requests_total`, `traefik_service_request_duration_seconds` (a histogram),
//! `traefik_service_requests_bytes_total` and `traefik_service_responses_bytes_total`, all
//! labelled with `service` (`app-7@docker`) and, for all but the byte counters of a
//! histogram, `code`.
//!
//! Everything here is a pure function over the text, so the tests run on fixtures. The
//! counters are returned as they are (cumulative); the server turns two samples into rates
//! and quantiles.

use std::collections::BTreeMap;
use std::time::Duration;

use proto::{HistogramBucket, HttpStats};

use crate::ingress::{METRICS_ADDRESS, METRICS_PORT, TRAEFIK_CONTAINER_NAME};
use crate::podman::Podman;
use crate::stats::resource_of;

const SCRAPE_TIMEOUT: Duration = Duration::from_secs(4);
/// A scrape is a few KB per service; anything bigger is not Traefik talking.
const MAX_BODY: usize = 8 << 20;

/// One line of the exposition format: `name{label="value",...} number`.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
	/// Metric name, `_bucket` suffix included for histogram buckets.
	pub name: String,
	/// Label pairs in file order.
	pub labels: Vec<(String, String)>,
	/// The reading; `+Inf` and `NaN` parse.
	pub value: f64,
}

impl Sample {
	/// The value of label `key`.
	#[must_use]
	pub fn label(&self, key: &str) -> Option<&str> {
		self.labels
			.iter()
			.find(|(k, _)| k == key)
			.map(|(_, v)| v.as_str())
	}
}

/// Parses the text format. Comment lines (`# HELP`, `# TYPE`), blank lines and lines that do
/// not parse are skipped: a scrape with one odd line still yields the others.
#[must_use]
pub fn parse(text: &str) -> Vec<Sample> {
	text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Sample> {
	let line = line.trim();
	if line.is_empty() || line.starts_with('#') {
		return None;
	}
	let name_end = line.find(|c: char| c == '{' || c.is_whitespace())?;
	let name = &line[..name_end];
	if name.is_empty() {
		return None;
	}
	let mut rest = line[name_end..].trim_start();
	let mut labels = Vec::new();
	if let Some(body) = rest.strip_prefix('{') {
		let (parsed, after) = parse_labels(body)?;
		labels = parsed;
		rest = after.trim_start();
	}
	// The value, then an optional millisecond timestamp.
	let value = parse_number(rest.split_whitespace().next()?)?;
	Some(Sample {
		name: name.to_owned(),
		labels,
		value,
	})
}

/// Reads `key="value",...}` and returns the labels and what follows the closing brace.
/// Values may hold escaped quotes, backslashes and newlines, and commas or braces.
fn parse_labels(mut s: &str) -> Option<(Vec<(String, String)>, &str)> {
	let mut labels = Vec::new();
	loop {
		s = s.trim_start();
		if let Some(after) = s.strip_prefix('}') {
			return Some((labels, after));
		}
		let eq = s.find('=')?;
		let key = s[..eq].trim().to_owned();
		s = s[eq + 1..].trim_start().strip_prefix('"')?;
		let mut value = String::new();
		let mut chars = s.char_indices();
		let end = loop {
			let (i, c) = chars.next()?;
			match c {
				'"' => break i,
				'\\' => match chars.next()?.1 {
					'n' => value.push('\n'),
					other => value.push(other),
				},
				other => value.push(other),
			}
		};
		labels.push((key, value));
		s = s[end + 1..].trim_start();
		s = s.strip_prefix(',').unwrap_or(s);
	}
}

fn parse_number(s: &str) -> Option<f64> {
	match s {
		"+Inf" | "Inf" => Some(f64::INFINITY),
		"-Inf" => Some(f64::NEG_INFINITY),
		"NaN" => Some(f64::NAN),
		_ => s.parse().ok(),
	}
}

/// The Traefik service name without its provider: `app-7@docker` is `app-7`.
fn service_name(label: &str) -> &str {
	label.split_once('@').map_or(label, |(name, _)| name)
}

/// Per-service counters of podkit's own services. Traefik's internal services
/// (`api@internal`, `prometheus@internal`) and anything that is not a podkit resource are
/// left out.
#[must_use]
pub fn http_stats(samples: &[Sample]) -> Vec<HttpStats> {
	#[derive(Default)]
	struct Acc {
		classes: [f64; 4],
		bytes_in: f64,
		bytes_out: f64,
		// `le` as text -> (bound, count); text keys keep `+Inf` and floats apart.
		buckets: BTreeMap<String, (f64, f64)>,
	}
	let mut by_service: BTreeMap<String, Acc> = BTreeMap::new();
	for sample in samples {
		let Some(service) = sample.label("service").map(service_name) else {
			continue;
		};
		if resource_of(service).is_none() || !sample.value.is_finite() {
			continue;
		}
		match sample.name.as_str() {
			"traefik_service_requests_total" => {
				let class = match sample.label("code").and_then(|c| c.bytes().next()) {
					Some(b'2') => 0,
					Some(b'3') => 1,
					Some(b'4') => 2,
					Some(b'5') => 3,
					_ => continue,
				};
				by_service.entry(service.to_owned()).or_default().classes[class] += sample.value;
			}
			"traefik_service_requests_bytes_total" => {
				by_service.entry(service.to_owned()).or_default().bytes_in += sample.value;
			}
			"traefik_service_responses_bytes_total" => {
				by_service.entry(service.to_owned()).or_default().bytes_out += sample.value;
			}
			"traefik_service_request_duration_seconds_bucket" => {
				let Some(le) = sample.label("le") else {
					continue;
				};
				let Some(bound) = parse_number(le) else {
					continue;
				};
				let slot = by_service
					.entry(service.to_owned())
					.or_default()
					.buckets
					.entry(le.to_owned())
					.or_insert((bound, 0.0));
				slot.1 += sample.value;
			}
			_ => {}
		}
	}
	by_service
		.into_iter()
		.filter_map(|(service, acc)| {
			let mut buckets: Vec<(f64, f64)> = acc.buckets.into_values().collect();
			buckets.sort_by(|a, b| a.0.total_cmp(&b.0));
			Some(HttpStats {
				resource: Some(resource_of(&service)?),
				r2xx: acc.classes[0],
				r3xx: acc.classes[1],
				r4xx: acc.classes[2],
				r5xx: acc.classes[3],
				bytes_in: acc.bytes_in,
				bytes_out: acc.bytes_out,
				buckets: buckets
					.into_iter()
					.map(|(le, count)| HistogramBucket {
						le: le.is_finite().then_some(le),
						count,
					})
					.collect(),
			})
		})
		.collect()
}

/// Reads Traefik's metrics. Empty when Traefik is not running, has no metrics yet (an older
/// container started before the metrics entrypoint existed) or does not answer in time:
/// the charts just have no HTTP points for that tick.
pub async fn scrape(podman: &Podman) -> Vec<HttpStats> {
	let url = format!("http://{METRICS_ADDRESS}:{METRICS_PORT}/metrics");
	let argv = ["wget", "-q", "-O", "-", url.as_str()];
	let run = podman.exec_output(TRAEFIK_CONTAINER_NAME, &argv);
	let Ok(Ok((0, body))) = tokio::time::timeout(SCRAPE_TIMEOUT, run).await else {
		return Vec::new();
	};
	if body.len() > MAX_BODY {
		return Vec::new();
	}
	http_stats(&parse(&body))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// What Traefik v3 prints for one app: two statuses and a histogram, plus a service
	/// that is not podkit's.
	const FIXTURE: &str = r#"# HELP traefik_service_requests_total How many HTTP requests processed on a service, partitioned by status code, protocol, and method.
# TYPE traefik_service_requests_total counter
traefik_service_requests_total{code="200",method="GET",protocol="http",service="app-7@docker"} 90
traefik_service_requests_total{code="201",method="POST",protocol="http",service="app-7@docker"} 4
traefik_service_requests_total{code="404",method="GET",protocol="http",service="app-7@docker"} 10
traefik_service_requests_total{code="502",method="GET",protocol="http",service="app-7@docker"} 1
traefik_service_requests_total{code="301",method="GET",protocol="http",service="stack-3-web@docker"} 2
traefik_service_requests_total{code="200",method="GET",protocol="http",service="api@internal"} 9
# TYPE traefik_service_request_duration_seconds histogram
traefik_service_request_duration_seconds_bucket{code="200",method="GET",protocol="http",service="app-7@docker",le="0.1"} 80
traefik_service_request_duration_seconds_bucket{code="200",method="GET",protocol="http",service="app-7@docker",le="1"} 90
traefik_service_request_duration_seconds_bucket{code="200",method="GET",protocol="http",service="app-7@docker",le="+Inf"} 90
traefik_service_request_duration_seconds_bucket{code="404",method="GET",protocol="http",service="app-7@docker",le="0.1"} 10
traefik_service_request_duration_seconds_bucket{code="404",method="GET",protocol="http",service="app-7@docker",le="1"} 10
traefik_service_request_duration_seconds_bucket{code="404",method="GET",protocol="http",service="app-7@docker",le="+Inf"} 10
traefik_service_request_duration_seconds_sum{code="200",method="GET",protocol="http",service="app-7@docker"} 4.5
traefik_service_requests_bytes_total{code="200",method="GET",protocol="http",service="app-7@docker"} 1000
traefik_service_requests_bytes_total{code="404",method="GET",protocol="http",service="app-7@docker"} 200
traefik_service_responses_bytes_total{code="200",method="GET",protocol="http",service="app-7@docker"} 51200
go_goroutines 17
"#;

	#[test]
	fn prometheus_text_parses_names_labels_and_values() {
		let samples = parse(FIXTURE);
		let first = &samples[0];
		assert_eq!(first.name, "traefik_service_requests_total");
		assert_eq!(first.label("code"), Some("200"));
		assert_eq!(first.label("service"), Some("app-7@docker"));
		assert!((first.value - 90.0).abs() < f64::EPSILON);
		// A metric without labels, and comments skipped.
		assert!(
			samples
				.iter()
				.any(|s| s.name == "go_goroutines" && s.labels.is_empty())
		);
		assert!(samples.iter().all(|s| !s.name.starts_with('#')));
	}

	#[test]
	fn prometheus_label_values_may_hold_escapes_commas_and_braces() {
		let samples = parse(
			"m{a=\"x,y}\",b=\"q\\\"uote\\\\ \\nline\"} 1.5e3 1700000000000\nm2{} +Inf\nbroken{a=\"1\" 3\nm3 NaN\n",
		);
		assert_eq!(samples[0].label("a"), Some("x,y}"));
		assert_eq!(samples[0].label("b"), Some("q\"uote\\ \nline"));
		assert!((samples[0].value - 1500.0).abs() < f64::EPSILON);
		assert!(samples[1].value.is_infinite());
		assert_eq!(samples.len(), 3, "the broken line is skipped: {samples:?}");
		assert!(samples[2].value.is_nan());
	}

	#[test]
	fn prometheus_service_counters_group_by_class_and_sum_the_histogram() {
		let stats = http_stats(&parse(FIXTURE));
		assert_eq!(stats.len(), 2, "api@internal is not a podkit service");
		let app = &stats[0];
		let resource = app.resource.as_ref().unwrap();
		assert_eq!((resource.kind.as_str(), resource.id), ("application", 7));
		assert_eq!(
			(app.r2xx, app.r3xx, app.r4xx, app.r5xx),
			(94.0, 0.0, 10.0, 1.0)
		);
		assert!((app.bytes_in - 1200.0).abs() < f64::EPSILON);
		assert!((app.bytes_out - 51200.0).abs() < f64::EPSILON);
		// The two statuses' buckets add up, in `le` order with +Inf last.
		let counts: Vec<(Option<f64>, f64)> = app.buckets.iter().map(|b| (b.le, b.count)).collect();
		assert_eq!(
			counts,
			[(Some(0.1), 90.0), (Some(1.0), 100.0), (None, 100.0)]
		);
		let stack = &stats[1];
		assert_eq!(
			stack.resource.as_ref().unwrap().service.as_deref(),
			Some("web")
		);
		assert!((stack.r3xx - 2.0).abs() < f64::EPSILON);
	}

	#[test]
	fn prometheus_without_service_metrics_gives_nothing() {
		assert!(http_stats(&parse("go_goroutines 3\n")).is_empty());
		assert!(http_stats(&parse("")).is_empty());
	}
}
