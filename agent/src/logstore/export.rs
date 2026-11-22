//! Forwarding stored lines to Loki or an OTLP/HTTP collector, best effort.
//!
//! The collector never waits for the backend: lines go into a bounded channel with
//! `try_send`, and a full channel drops the line and counts it. One task batches the channel
//! (500 lines or 5 s) and posts; a slow or failing backend costs dropped lines, not memory
//! and not collection.
//!
//! Wire formats, from the projects' own documentation:
//! - Loki push, JSON body: <https://grafana.com/docs/loki/latest/reference/loki-http-api/#ingest-logs>
//!   (`POST /loki/api/v1/push`, `{"streams":[{"stream":{labels},"values":[["<unix ns>","<line>"]]}]}`,
//!   `Content-Type: application/json`, `X-Scope-OrgID` for a tenant).
//! - OTLP/HTTP with JSON encoding: <https://opentelemetry.io/docs/specs/otlp/#otlphttp> and
//!   `opentelemetry/proto/logs/v1/logs.proto`: `POST /v1/logs`, `{"resourceLogs":[{"resource":
//!   {"attributes":[..]},"scopeLogs":[{"scope":{"name":..},"logRecords":[..]}]}]}`; 64-bit
//!   integers are decimal strings, enums are integers, attribute values are
//!   `{"stringValue":..}` / `{"intValue":"..."}`. 429, 502, 503 and 504 are retryable.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use proto::{LogExport, LogExportKind, LogLevel, LogRecord, Stream};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Lines waiting for the backend; more than this are dropped.
pub const QUEUE: usize = 1024;
const BATCH_LINES: usize = 500;
const BATCH_WAIT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const RETRIES: u32 = 3;

/// One line on its way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
	/// The `podkit.service` the line belongs to.
	pub svc: String,
	/// The line.
	pub record: LogRecord,
}

/// The URL a payload is posted to: `url` as given when it already ends with the protocol's
/// path, otherwise with that path added, so `http://loki:3100` works.
#[must_use]
pub fn endpoint(export: &LogExport) -> String {
	let url = export.url.trim_end_matches('/');
	let path = match export.kind {
		LogExportKind::Loki => "/loki/api/v1/push",
		LogExportKind::Otlp => "/v1/logs",
	};
	if url.ends_with(path) {
		url.to_owned()
	} else {
		format!("{url}{path}")
	}
}

fn stream_name(stream: Stream) -> &'static str {
	match stream {
		Stream::Stdout => "stdout",
		Stream::Stderr => "stderr",
		Stream::System => "system",
	}
}

fn nanos(ts_ms: i64) -> String {
	(i128::from(ts_ms) * 1_000_000).to_string()
}

/// The Loki push body: one stream per distinct label set, its lines oldest first. Labels are
/// `job`, the team and server labels of the config, the service, the stream, the level when
/// the line named one and the deployment when it had one.
#[must_use]
pub fn loki_payload(export: &LogExport, lines: &[Outgoing]) -> Value {
	let mut streams: BTreeMap<BTreeMap<String, String>, Vec<(i64, &str)>> = BTreeMap::new();
	for Outgoing { svc, record } in lines {
		let mut labels: BTreeMap<String, String> = BTreeMap::new();
		labels.insert("job".into(), "podkit".into());
		for (k, v) in &export.labels {
			labels.insert(k.clone(), v.clone());
		}
		labels.insert("service".into(), svc.clone());
		labels.insert("stream".into(), stream_name(record.stream).into());
		if let Some(level) = record.level {
			labels.insert("level".into(), level.as_str().into());
		}
		if let Some(dep) = record.dep {
			labels.insert("deployment".into(), dep.to_string());
		}
		streams
			.entry(labels)
			.or_default()
			.push((record.ts, &record.line));
	}
	let streams: Vec<Value> = streams
		.into_iter()
		.map(|(labels, mut values)| {
			values.sort_by_key(|(ts, _)| *ts);
			json!({
				"stream": labels,
				"values": values
					.into_iter()
					.map(|(ts, line)| json!([nanos(ts), line]))
					.collect::<Vec<_>>(),
			})
		})
		.collect();
	json!({ "streams": streams })
}

/// `SeverityNumber` of `logs.proto`: the first number of each band.
fn severity(level: Option<LogLevel>) -> (u8, &'static str) {
	match level {
		Some(LogLevel::Debug) => (5, "DEBUG"),
		Some(LogLevel::Info) | None => (9, "INFO"),
		Some(LogLevel::Warn) => (13, "WARN"),
		Some(LogLevel::Error) => (17, "ERROR"),
	}
}

fn attr(key: &str, value: &str) -> Value {
	json!({ "key": key, "value": { "stringValue": value } })
}

/// The OTLP/JSON `ExportLogsServiceRequest`: one resource (this agent's host, from the
/// config's labels), one scope, one record per line. A line that named no level is reported
/// with severity `INFO` (9) like the search treats it; its text is the body.
#[must_use]
pub fn otlp_payload(export: &LogExport, lines: &[Outgoing], observed_ms: i64) -> Value {
	let mut resource = vec![attr("service.name", "podkit")];
	for (k, v) in &export.labels {
		resource.push(attr(&format!("podkit.{k}"), v));
	}
	let records: Vec<Value> = lines
		.iter()
		.map(|Outgoing { svc, record }| {
			let (number, text) = severity(record.level);
			let mut attributes = vec![
				attr("podkit.service", svc),
				attr("log.iostream", stream_name(record.stream)),
			];
			if let Some(dep) = record.dep {
				attributes.push(json!({
					"key": "podkit.deployment.id",
					"value": { "intValue": dep.to_string() },
				}));
			}
			json!({
				"timeUnixNano": nanos(record.ts),
				"observedTimeUnixNano": nanos(observed_ms),
				"severityNumber": number,
				"severityText": text,
				"body": { "stringValue": record.line },
				"attributes": attributes,
			})
		})
		.collect();
	json!({
		"resourceLogs": [{
			"resource": { "attributes": resource },
			"scopeLogs": [{
				"scope": { "name": "podkit-agent" },
				"logRecords": records,
			}],
		}],
	})
}

/// The body for `export`'s protocol.
#[must_use]
pub fn payload(export: &LogExport, lines: &[Outgoing], now_ms: i64) -> Value {
	match export.kind {
		LogExportKind::Loki => loki_payload(export, lines),
		LogExportKind::Otlp => otlp_payload(export, lines, now_ms),
	}
}

/// Why a batch was not delivered.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
	#[error("{0}")]
	/// The request did not get an answer.
	Transport(String),
	/// The backend refused the data for good (a 4xx other than 429).
	#[error("the backend answered {0}")]
	Refused(u16),
	/// The backend is busy or down (429, 5xx): worth trying again.
	#[error("the backend answered {0}")]
	Busy(u16),
}

impl SendError {
	fn retryable(&self) -> bool {
		matches!(self, Self::Transport(_) | Self::Busy(_))
	}
}

/// Posts one batch.
///
/// # Errors
/// The backend is unreachable or does not accept the batch.
pub async fn send(
	http: &reqwest::Client,
	export: &LogExport,
	lines: &[Outgoing],
	now_ms: i64,
) -> Result<(), SendError> {
	let mut request = http
		.post(endpoint(export))
		.timeout(REQUEST_TIMEOUT)
		.json(&payload(export, lines, now_ms));
	if let Some(auth) = &export.authorization {
		request = request.header(reqwest::header::AUTHORIZATION, auth);
	}
	if let (LogExportKind::Loki, Some(tenant)) = (export.kind, &export.tenant) {
		request = request.header("X-Scope-OrgID", tenant);
	}
	let response = request
		.send()
		.await
		.map_err(|e| SendError::Transport(e.without_url().to_string()))?;
	let status = response.status().as_u16();
	match status {
		200..=299 => Ok(()),
		429 | 500..=599 => Err(SendError::Busy(status)),
		_ => Err(SendError::Refused(status)),
	}
}

/// [`send`] with up to [`RETRIES`] retries after 1, 2 and 4 seconds (scaled by `pause`) for
/// a backend that is busy.
///
/// # Errors
/// The last error.
pub async fn send_with_retries(
	http: &reqwest::Client,
	export: &LogExport,
	lines: &[Outgoing],
	pause: Duration,
) -> Result<(), SendError> {
	let mut attempt = 0;
	loop {
		let now = super::files::now();
		match send(http, export, lines, now).await {
			Ok(()) => return Ok(()),
			Err(e) if e.retryable() && attempt < RETRIES => {
				tokio::time::sleep(pause * 2u32.pow(attempt)).await;
				attempt += 1;
			}
			Err(e) => return Err(e),
		}
	}
}

/// Counters the status shows.
#[derive(Debug, Default)]
pub struct Counters {
	/// Lines dropped: the queue was full or the backend did not take them.
	pub dropped: AtomicU64,
}

/// The running exporter: where lines are offered, and the task that sends them.
pub struct Exporter {
	tx: mpsc::Sender<Outgoing>,
	task: tokio::task::JoinHandle<()>,
	counters: Arc<Counters>,
}

impl Exporter {
	/// Starts forwarding to `export`. Lines already counted as dropped stay counted.
	#[must_use]
	pub fn start(export: LogExport, counters: Arc<Counters>) -> Self {
		let (tx, rx) = mpsc::channel(QUEUE);
		let task = tokio::spawn(run(
			export,
			rx,
			Arc::clone(&counters),
			Duration::from_secs(1),
		));
		Self { tx, task, counters }
	}

	/// Offers a line without waiting. A full queue drops it.
	pub fn offer(&self, line: Outgoing) {
		if self.tx.try_send(line).is_err() {
			self.counters.dropped.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for Exporter {
	fn drop(&mut self) {
		self.task.abort();
	}
}

async fn run(
	export: LogExport,
	mut rx: mpsc::Receiver<Outgoing>,
	counters: Arc<Counters>,
	pause: Duration,
) {
	let http = match reqwest::Client::builder().build() {
		Ok(http) => http,
		Err(e) => {
			tracing::warn!(error = %e, "log export: no HTTP client, forwarding is off");
			return;
		}
	};
	let mut batch: Vec<Outgoing> = Vec::with_capacity(BATCH_LINES);
	loop {
		let Some(first) = rx.recv().await else { return };
		batch.push(first);
		let deadline = tokio::time::Instant::now() + BATCH_WAIT;
		while batch.len() < BATCH_LINES {
			match tokio::time::timeout_at(deadline, rx.recv()).await {
				Ok(Some(line)) => batch.push(line),
				Ok(None) | Err(_) => break,
			}
		}
		if let Err(e) = send_with_retries(&http, &export, &batch, pause).await {
			counters
				.dropped
				.fetch_add(batch.len() as u64, Ordering::Relaxed);
			tracing::warn!(error = %e, lines = batch.len(), "log export: a batch was dropped");
		}
		batch.clear();
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Mutex;

	use axum::Router;
	use axum::body::Bytes;
	use axum::http::{HeaderMap, StatusCode};
	use axum::routing::post;

	use super::*;

	fn export(kind: LogExportKind, url: &str) -> LogExport {
		LogExport {
			kind,
			url: url.to_owned(),
			authorization: Some("Bearer s3cret".into()),
			tenant: Some("team-1".into()),
			labels: vec![
				("team".into(), "acme".into()),
				("server".into(), "box".into()),
			],
		}
	}

	fn line(svc: &str, ts: i64, level: Option<LogLevel>, dep: Option<i64>, text: &str) -> Outgoing {
		Outgoing {
			svc: svc.to_owned(),
			record: LogRecord {
				ts,
				stream: Stream::Stderr,
				dep,
				level,
				line: text.to_owned(),
				svc: String::new(),
			},
		}
	}

	#[test]
	fn logstore_the_endpoint_gets_its_protocol_path() {
		let loki = export(LogExportKind::Loki, "http://loki:3100/");
		assert_eq!(endpoint(&loki), "http://loki:3100/loki/api/v1/push");
		let full = export(LogExportKind::Loki, "http://loki:3100/loki/api/v1/push");
		assert_eq!(endpoint(&full), "http://loki:3100/loki/api/v1/push");
		let otlp = export(LogExportKind::Otlp, "https://otel.example/");
		assert_eq!(endpoint(&otlp), "https://otel.example/v1/logs");
		let prefixed = export(LogExportKind::Otlp, "https://x/otlp");
		assert_eq!(endpoint(&prefixed), "https://x/otlp/v1/logs");
	}

	#[test]
	fn logstore_loki_payload_groups_lines_into_labelled_streams_with_nanosecond_strings() {
		let lines = [
			line(
				"app-1",
				1_700_000_000_002,
				Some(LogLevel::Error),
				Some(12),
				"second",
			),
			line(
				"app-1",
				1_700_000_000_001,
				Some(LogLevel::Error),
				Some(12),
				"first",
			),
			line("db-2", 1_700_000_000_003, None, None, "no level"),
		];
		let body = loki_payload(&export(LogExportKind::Loki, "http://x"), &lines);
		let streams = body["streams"].as_array().unwrap();
		assert_eq!(streams.len(), 2);
		let app = streams
			.iter()
			.find(|s| s["stream"]["service"] == "app-1")
			.unwrap();
		assert_eq!(
			app["stream"],
			json!({
				"job": "podkit", "team": "acme", "server": "box", "service": "app-1",
				"stream": "stderr", "level": "error", "deployment": "12",
			})
		);
		assert_eq!(
			app["values"],
			json!([
				["1700000000001000000", "first"],
				["1700000000002000000", "second"],
			])
		);
		let db = streams
			.iter()
			.find(|s| s["stream"]["service"] == "db-2")
			.unwrap();
		assert!(db["stream"].get("level").is_none());
		assert!(db["stream"].get("deployment").is_none());
	}

	#[test]
	fn logstore_otlp_payload_follows_the_json_encoding_rules() {
		let lines = [
			line(
				"app-1",
				1_700_000_000_123,
				Some(LogLevel::Warn),
				Some(12),
				"slow",
			),
			line("db-2", 1_700_000_000_456, None, None, "ready"),
		];
		let body = otlp_payload(
			&export(LogExportKind::Otlp, "http://x"),
			&lines,
			1_700_000_001_000,
		);
		let resource = &body["resourceLogs"][0];
		assert_eq!(
			resource["resource"]["attributes"],
			json!([
				{ "key": "service.name", "value": { "stringValue": "podkit" } },
				{ "key": "podkit.team", "value": { "stringValue": "acme" } },
				{ "key": "podkit.server", "value": { "stringValue": "box" } },
			])
		);
		let scope = &resource["scopeLogs"][0];
		assert_eq!(scope["scope"]["name"], "podkit-agent");
		let records = scope["logRecords"].as_array().unwrap();
		assert_eq!(records.len(), 2);
		assert_eq!(records[0]["timeUnixNano"], "1700000000123000000");
		assert_eq!(records[0]["observedTimeUnixNano"], "1700000001000000000");
		assert_eq!(records[0]["severityNumber"], 13);
		assert_eq!(records[0]["severityText"], "WARN");
		assert_eq!(records[0]["body"], json!({ "stringValue": "slow" }));
		assert_eq!(
			records[0]["attributes"],
			json!([
				{ "key": "podkit.service", "value": { "stringValue": "app-1" } },
				{ "key": "log.iostream", "value": { "stringValue": "stderr" } },
				{ "key": "podkit.deployment.id", "value": { "intValue": "12" } },
			])
		);
		assert_eq!(records[1]["severityNumber"], 9);
		// Field names are lowerCamelCase: no snake_case key anywhere.
		assert!(!body.to_string().contains("time_unix_nano"));
	}

	#[derive(Clone, Default)]
	struct Stub {
		seen: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
		fail_first: Arc<AtomicU64>,
		status: Arc<Mutex<u16>>,
	}

	/// A backend on a loopback port answering every POST like Loki or a collector would.
	async fn stub(stub: Stub) -> String {
		async fn handle(
			axum::extract::State(stub): axum::extract::State<Stub>,
			uri: axum::http::Uri,
			headers: HeaderMap,
			body: Bytes,
		) -> StatusCode {
			if stub.fail_first.load(Ordering::SeqCst) > 0 {
				stub.fail_first.fetch_sub(1, Ordering::SeqCst);
				return StatusCode::SERVICE_UNAVAILABLE;
			}
			stub.seen.lock().unwrap().push((
				uri.path().to_owned(),
				headers,
				serde_json::from_slice(&body).unwrap(),
			));
			let code = *stub.status.lock().unwrap();
			StatusCode::from_u16(if code == 0 { 204 } else { code }).unwrap()
		}
		let app = Router::new().fallback(post(handle)).with_state(stub);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
		format!("http://{addr}")
	}

	fn client() -> reqwest::Client {
		let _ = rustls::crypto::ring::default_provider().install_default();
		reqwest::Client::new()
	}

	#[tokio::test]
	async fn logstore_a_stub_loki_receives_the_push_with_its_headers() {
		let backend = Stub::default();
		let url = stub(backend.clone()).await;
		let export = export(LogExportKind::Loki, &url);
		let lines = [line("app-1", 1_700_000_000_001, None, Some(3), "hello")];
		send(&client(), &export, &lines, 1).await.unwrap();

		let seen = backend.seen.lock().unwrap();
		assert_eq!(seen.len(), 1);
		let (path, headers, body) = &seen[0];
		assert_eq!(path, "/loki/api/v1/push");
		assert_eq!(headers["content-type"], "application/json");
		assert_eq!(headers["authorization"], "Bearer s3cret");
		assert_eq!(headers["x-scope-orgid"], "team-1");
		assert_eq!(
			body["streams"][0]["values"][0],
			json!(["1700000000001000000", "hello"])
		);
	}

	#[tokio::test]
	async fn logstore_a_stub_collector_receives_otlp_on_v1_logs_without_a_tenant_header() {
		let backend = Stub::default();
		let url = stub(backend.clone()).await;
		let export = export(LogExportKind::Otlp, &url);
		let lines = [line(
			"app-1",
			1_700_000_000_001,
			Some(LogLevel::Error),
			None,
			"boom",
		)];
		send(&client(), &export, &lines, 1_700_000_000_500)
			.await
			.unwrap();

		let seen = backend.seen.lock().unwrap();
		let (path, headers, body) = &seen[0];
		assert_eq!(path, "/v1/logs");
		assert!(headers.get("x-scope-orgid").is_none());
		assert_eq!(
			body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"],
			"boom"
		);
	}

	#[tokio::test]
	async fn logstore_a_busy_backend_is_retried_and_a_refusing_one_is_not() {
		let backend = Stub::default();
		backend.fail_first.store(2, Ordering::SeqCst);
		let url = stub(backend.clone()).await;
		let export = export(LogExportKind::Loki, &url);
		let lines = [line("app-1", 1, None, None, "x")];
		send_with_retries(&client(), &export, &lines, Duration::from_millis(1))
			.await
			.unwrap();
		assert_eq!(backend.seen.lock().unwrap().len(), 1);

		let refusing = Stub::default();
		*refusing.status.lock().unwrap() = 400;
		let url = stub(refusing.clone()).await;
		let err = send_with_retries(
			&client(),
			&self::export(LogExportKind::Loki, &url),
			&lines,
			Duration::from_millis(1),
		)
		.await
		.unwrap_err();
		assert!(matches!(err, SendError::Refused(400)), "{err:?}");
		assert_eq!(refusing.seen.lock().unwrap().len(), 1, "no retry on a 400");
	}

	#[tokio::test]
	async fn logstore_a_full_queue_drops_lines_instead_of_waiting() {
		let counters = Arc::new(Counters::default());
		// Nothing listens on this port: the sender stalls retrying while the queue fills.
		let exporter = Exporter::start(
			export(LogExportKind::Loki, "http://127.0.0.1:1"),
			Arc::clone(&counters),
		);
		let started = std::time::Instant::now();
		for i in 0..(QUEUE * 4) {
			exporter.offer(line("app-1", i64::try_from(i).unwrap(), None, None, "x"));
		}
		assert!(
			started.elapsed() < Duration::from_secs(2),
			"offering must not block"
		);
		assert!(counters.dropped.load(Ordering::Relaxed) > 0);
	}
}
