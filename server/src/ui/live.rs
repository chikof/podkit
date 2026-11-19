//! Live log streams for Datastar. The JSON endpoints stream `event: log` with
//! `{stream,line}`, which Datastar cannot render; these adapters turn the same
//! sources (a job's log, an agent's container stream) into
//! `datastar-patch-elements` events that append one `<div>` per line.
//!
//! Lines that arrive within [`WINDOW`] go out as one patch, so a burst of
//! thousands of lines costs a few events. When the source ends (or the server
//! shuts down) the stream sends the signal `_live: false`.

use std::convert::Infallible;
use std::time::Duration;

use axum::response::{IntoResponse, Response, Sse, sse::Event, sse::KeepAlive};
use datastar::consts::ElementPatchMode;
use datastar::prelude::*;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream, Stream};
use maud::{Markup, html};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use super::sse::js_string;
use crate::applications::routes::{AgentLogStep, agent_log_step};
use crate::error::ServerError;
use crate::ids::JobId;
use crate::logs::{Frame, tagged_frames};
use crate::state::AppState;

/// How long a batch waits for more lines after its first.
const WINDOW: Duration = Duration::from_millis(50);
/// A batch is flushed early at this many lines or bytes of text.
const MAX_BATCH_LINES: usize = 200;
const MAX_BATCH_BYTES: usize = 64 * 1024;
/// A job log replays at most this many stored lines (same as `?tail=` on
/// container logs).
const MAX_REPLAY_LINES: usize = 5000;

/// One thing to show in a log view.
enum Item {
	Line {
		stream: proto::Stream,
		line: String,
		/// Unix milliseconds when the source stamped it; `None` for a live container line,
		/// which is stamped on arrival.
		ts: Option<i64>,
	},
	/// Not part of the log itself: "N lines skipped", "log truncated".
	Notice(String),
}

/// Streams a job's log: stored lines first, then live ones until the job
/// finishes. `None` (no job yet) ends at once. Lines append to `#target_id`.
pub(crate) fn job_log(state: &AppState, job: Option<JobId>, target_id: &str) -> Response {
	// Subscribing before the replay opens the file is what makes the merge lossless.
	let (path, rx) = match job {
		Some(id) => (Some(state.logs.path(id)), state.logs.subscribe(id)),
		None => (None, None),
	};
	let mut replayed = 0usize;
	let items = tagged_frames(path, rx)
		.flat_map(move |(frame, from_file)| {
			let items = match frame {
				Frame::Line(l) if from_file => {
					replayed += 1;
					match replayed {
						n if n <= MAX_REPLAY_LINES => vec![Item::Line {
							stream: l.stream,
							line: l.line,
							ts: Some(l.ts),
						}],
						n if n == MAX_REPLAY_LINES + 1 => vec![Item::Notice(format!(
							"log truncated: only the first {MAX_REPLAY_LINES} stored lines are shown"
						))],
						_ => Vec::new(),
					}
				}
				Frame::Line(l) => vec![Item::Line {
					stream: l.stream,
					line: l.line,
					ts: Some(l.ts),
				}],
				Frame::Lagged(n) => vec![Item::Notice(format!(
					"{n} lines were skipped because the browser fell behind"
				))],
				Frame::End => Vec::new(),
			};
			stream::iter(items)
		})
		.boxed();
	respond(items, target_id, state.shutdown.clone())
}

/// Streams a container's log from the agent into `#target_id`. The tail the
/// caller asked the agent for is the replay, so no extra cap applies.
pub(crate) fn container_log(
	events: BoxStream<'static, Result<proto::Event, ServerError>>,
	target_id: &str,
	shutdown: CancellationToken,
) -> Response {
	let items = stream::unfold(Some(events), |events| async move {
		let mut events = events?;
		loop {
			match agent_log_step(events.next().await) {
				AgentLogStep::Line { stream, line } => {
					let item = Item::Line {
						stream,
						line,
						ts: None,
					};
					return Some((vec![item], Some(events)));
				}
				AgentLogStep::Skip => {}
				AgentLogStep::End { failed: false } => return None,
				AgentLogStep::End { failed: true } => {
					let notice = Item::Notice("log stream interrupted".to_owned());
					return Some((vec![notice], None));
				}
			}
		}
	})
	.flat_map(stream::iter)
	.boxed();
	respond(items, target_id, shutdown)
}

fn respond(
	items: BoxStream<'static, Item>,
	target_id: &str,
	shutdown: CancellationToken,
) -> Response {
	Sse::new(patches(items, target_id, shutdown))
		.keep_alive(KeepAlive::default())
		.into_response()
}

/// Groups `items` into append patches for `#target_id` and finishes with
/// `_live: false` when they run out or `shutdown` fires.
fn patches(
	items: BoxStream<'static, Item>,
	target_id: &str,
	shutdown: CancellationToken,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
	debug_assert!(
		target_id
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
		"target id {target_id:?} is not a plain CSS id"
	);
	let selector = format!("#{target_id}");
	let state = Some((items, shutdown));
	stream::unfold(state, move |state| {
		let selector = selector.clone();
		async move {
			let (mut items, shutdown) = state?;
			let mut batch: Vec<Item> = Vec::new();
			let mut bytes = 0usize;
			let mut deadline: Option<Instant> = None;
			let mut ended = false;
			while batch.len() < MAX_BATCH_LINES && bytes < MAX_BATCH_BYTES {
				let next = match deadline {
					None => tokio::select! {
						biased;
						() = shutdown.cancelled() => None,
						next = items.next() => next,
					},
					Some(at) => tokio::select! {
						biased;
						() = shutdown.cancelled() => None,
						next = timeout_at(at, items.next()) => match next {
							Ok(next) => next,
							Err(_) => break,
						},
					},
				};
				let Some(item) = next else {
					ended = true;
					break;
				};
				bytes += match &item {
					Item::Line { line, .. } => line.len(),
					Item::Notice(text) => text.len(),
				};
				batch.push(item);
				deadline.get_or_insert_with(|| Instant::now() + WINDOW);
			}

			let mut events = Vec::new();
			if !batch.is_empty() {
				events.push(append_event(&selector, &batch));
			}
			if ended {
				events.push(
					PatchSignals::new(r#"{"_live":false}"#.to_owned()).write_as_axum_sse_event(),
				);
			}
			let next = (!ended).then_some((items, shutdown));
			Some((stream::iter(events.into_iter().map(Ok)), next))
		}
	})
	.flatten()
}

fn append_event(selector: &str, batch: &[Item]) -> Event {
	let markup = html! {
		@for item in batch {
			(match item {
				Item::Line { stream, line, ts } => line_markup(*stream, line, *ts),
				Item::Notice(text) => notice_markup(text),
			})
		}
	};
	PatchElements::new(markup.0)
		.selector(selector)
		.mode(ElementPatchMode::Append)
		.write_as_axum_sse_event()
}

/// The level a line is shown at (`data-lv`): what the colour and the filter chips key on.
/// Podkit's own narration (the system stream) reads as a step; for the rest the words of the
/// line decide, because a container writes plain progress to stderr as often as an error.
fn level(stream: proto::Stream, line: &str) -> &'static str {
	let lower = line.to_ascii_lowercase();
	let has = |words: &[&str]| {
		lower
			.split(|c: char| !c.is_ascii_alphanumeric())
			.any(|w| words.contains(&w))
	};
	if has(&[
		"error",
		"err",
		"fatal",
		"panic",
		"failed",
		"failure",
		"exception",
		"denied",
		"oom",
	]) {
		"ERR"
	} else if has(&["warn", "warning", "deprecated"]) {
		"WARN"
	} else if has(&["debug", "trace"]) {
		"DEBUG"
	} else if line.starts_with('\u{2713}')
		|| lower.starts_with("successfully")
		|| lower.contains("health check passed")
	{
		"OK"
	} else if matches!(stream, proto::Stream::System)
		|| lower.starts_with("step ")
		|| (line.starts_with('[') && line.contains('/') && line.contains("] "))
	{
		"STEP"
	} else {
		"INFO"
	}
}

/// `12:04:31` in UTC: no locale, no script, the same everywhere.
fn clock(ts: Option<i64>) -> String {
	let at = ts
		.and_then(|ms| time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000)).ok())
		.unwrap_or_else(time::OffsetDateTime::now_utc);
	format!("{:02}:{:02}:{:02}", at.hour(), at.minute(), at.second())
}

/// One log line: time, level and text. Text goes through maud's escaping only.
fn line_markup(stream: proto::Stream, line: &str, ts: Option<i64>) -> Markup {
	let lv = level(stream, line);
	html! {
		div class="ui-log-line" data-lv=(lv) {
			span class="ui-log-ts" { (clock(ts)) }
			span class="ui-log-lv" { (lv) }
			span class="ui-log-tx" { (line) }
		}
	}
}

fn notice_markup(text: &str) -> Markup {
	html! {
		div class="ui-log-line" data-lv="DEBUG" {
			span class="ui-log-ts" { (clock(None)) }
			span class="ui-log-lv" { "NOTE" }
			span class="ui-log-tx" { (text) }
		}
	}
}

/// A line the page itself writes into the log when the stream cannot start (nothing is
/// running, the agent is down): an error row like any other, so it is filtered and read the
/// same way.
pub(crate) fn error_line(text: &str) -> Markup {
	html! {
		div class="ui-log-line" data-lv="ERR" {
			span class="ui-log-ts" { (clock(None)) }
			span class="ui-log-lv" { "ERR" }
			span class="ui-log-tx" { (text) }
		}
	}
}

/// A scrollable monospace log that starts streaming from `stream_url` as soon
/// as it loads. The endpoint must answer with [`job_log`] or [`container_log`]
/// for the same `id`.
///
/// Uses the signals `_live` (shown while streaming) and `_follow` (the
/// auto-scroll toggle), so a page holds one log view.
pub fn log_view(id: &str, stream_url: &str) -> Markup {
	let on_load = format!("@get({})", js_string(stream_url));
	html! {
		div class="flex flex-col gap-2" "data-signals"="{_live: true, _follow: true}" {
			div class="flex items-center justify-between text-xs text-muted-foreground" {
				span "data-text"="$_live ? 'Live' : 'Ended'" {}
				label class="flex cursor-pointer items-center gap-2" {
					input type="checkbox" "data-bind:_follow";
					"Auto-scroll"
				}
			}
			div class="h-96 overflow-auto rounded-lg border border-border bg-card p-3 font-mono text-xs leading-5 text-card-foreground"
				"data-init"="new MutationObserver(() => { if ($_follow) el.scrollTop = el.scrollHeight }).observe(el, {childList: true, subtree: true})" {
				div id=(id) "data-init"=(on_load) {}
			}
		}
	}
}

#[cfg(test)]
pub(super) fn test_router() -> axum::Router<AppState> {
	use axum::extract::{Path, State};
	use axum::routing::get;

	async fn job(State(s): State<AppState>, Path(id): Path<i64>) -> Response {
		job_log(&s, Some(JobId(id)), "log")
	}
	async fn view() -> Markup {
		html! {
			(log_view("log", "/_test/live/job/1"))
			(super::status::status_badge(crate::deployments::DeploymentStatus::Building))
			(super::status::poll("/_test/live/view", true))
		}
	}
	axum::Router::new()
		.route("/_test/live/job/{id}", get(job))
		.route("/_test/live/view", get(view))
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use crate::tests::PgPool;
	use axum::body::Body;
	use axum::http::{Request, header};
	use tower::ServiceExt;

	use super::*;
	use crate::tests::state;

	/// A pool that never connects: these tests don't touch the database.
	fn lazy_state() -> AppState {
		state(PgPool::connect_lazy("postgres://unused@127.0.0.1:1/unused").unwrap())
	}

	#[derive(Debug)]
	struct Sse {
		event: String,
		data: Vec<String>,
	}

	impl Sse {
		fn is_append_to(&self, selector: &str) -> bool {
			self.event == "datastar-patch-elements"
				&& self
					.data
					.iter()
					.any(|d| d == &format!("selector {selector}"))
				&& self.data.iter().any(|d| d == "mode append")
		}

		fn html(&self) -> String {
			self.data
				.iter()
				.filter_map(|d| d.strip_prefix("elements "))
				.collect()
		}
	}

	async fn read(resp: Response) -> Vec<Sse> {
		let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
			.await
			.unwrap();
		parse(core::str::from_utf8(&bytes).unwrap())
	}

	fn parse(text: &str) -> Vec<Sse> {
		text.split("\n\n")
			.filter(|block| block.contains("event:"))
			.map(|block| {
				let mut event = String::new();
				let mut data = Vec::new();
				for line in block.lines() {
					if let Some(e) = line.strip_prefix("event: ") {
						e.clone_into(&mut event);
					} else if let Some(d) = line.strip_prefix("data: ") {
						data.push(d.to_owned());
					}
				}
				Sse { event, data }
			})
			.collect()
	}

	/// The text of each line, in order, across all append patches (the `ui-log-tx` span).
	fn lines(events: &[Sse]) -> Vec<String> {
		events
			.iter()
			.filter(|e| e.is_append_to("#log"))
			.flat_map(|e| {
				let html = e.html();
				html.split("class=\"ui-log-tx\">")
					.skip(1)
					.filter_map(|d| d.split_once("</span>").map(|(text, _)| text.to_owned()))
					.collect::<Vec<_>>()
			})
			.collect()
	}

	fn ends_with_live_false(events: &[Sse]) -> bool {
		events.last().is_some_and(|e| {
			e.event == "datastar-patch-signals"
				&& e.data.iter().any(|d| d.contains(r#""_live":false"#))
		})
	}

	fn agent_lines(n: usize) -> BoxStream<'static, Result<proto::Event, ServerError>> {
		stream::iter((0..n).map(|i| {
			Ok(proto::Event::Log {
				stream: proto::Stream::Stdout,
				line: format!("line {i}"),
			})
		}))
		.chain(stream::once(async { Ok(proto::Event::Done) }))
		.boxed()
	}

	#[tokio::test]
	async fn replayed_and_live_lines_arrive_once_in_order() {
		let state = lazy_state();
		let id = JobId(7);
		let mut log = state.logs.open(id).await.unwrap();
		log.push(proto::Stream::Stdout, "one").await;
		log.push(proto::Stream::Stderr, "two").await;

		let resp = job_log(&state, Some(id), "log");
		let reader = tokio::spawn(read(resp));
		tokio::time::sleep(Duration::from_millis(200)).await;
		log.push(proto::Stream::System, "three").await;
		log.push(proto::Stream::Stdout, "four").await;
		log.finish().await;

		let events = reader.await.unwrap();
		assert_eq!(lines(&events), ["one", "two", "three", "four"]);
		let html: String = events.iter().map(Sse::html).collect();
		assert!(
			html.contains(r#"<div class="ui-log-line" data-lv="INFO">"#),
			"{html}"
		);
		// The system stream is podkit's own narration: a step.
		assert!(html.contains(r#"data-lv="STEP"><span"#), "{html}");
		assert!(ends_with_live_false(&events), "{events:?}");
	}

	#[test]
	fn the_level_comes_from_the_words_of_the_line() {
		use proto::Stream::{Stderr, Stdout, System};
		for (stream, line, want) in [
			(Stdout, "listening on :8080", "INFO"),
			(Stderr, "listening on :8080", "INFO"),
			(Stderr, "error: connection refused", "ERR"),
			(Stdout, "npm ERR! code ELIFECYCLE", "ERR"),
			(Stdout, "Build failed with 2 errors", "ERR"),
			(Stdout, "warn: image is 2 MB", "WARN"),
			(Stdout, "WARNING deprecated flag", "WARN"),
			(Stdout, "debug: cache miss", "DEBUG"),
			(Stdout, "\u{2713} Compiled successfully", "OK"),
			(Stdout, "Health check passed (3/3)", "OK"),
			(Stdout, "STEP 4/7: RUN pnpm build", "STEP"),
			(Stdout, "[2/7] WORKDIR /app", "STEP"),
			(System, "removing service db", "STEP"),
			// A word inside a longer word is not the level.
			(Stdout, "terror of the deep, warnings-as-docs", "INFO"),
		] {
			assert_eq!(level(stream, line), want, "{line}");
		}
	}

	#[test]
	fn a_line_carries_its_time_level_and_text_in_three_cells() {
		let html =
			line_markup(proto::Stream::Stdout, "error: <b>x</b>", Some(3_723_000)).into_string();
		assert!(
			html.contains(r#"<span class="ui-log-ts">01:02:03</span>"#),
			"{html}"
		);
		assert!(
			html.contains(r#"<span class="ui-log-lv">ERR</span>"#),
			"{html}"
		);
		assert!(html.contains("error: &lt;b&gt;x&lt;/b&gt;"), "{html}");
		assert!(!html.contains("<b>"), "{html}");
	}

	#[tokio::test]
	async fn no_job_ends_immediately() {
		let state = lazy_state();
		let events = read(job_log(&state, None, "log")).await;
		assert_eq!(events.len(), 1, "{events:?}");
		assert!(ends_with_live_false(&events));
	}

	#[tokio::test]
	async fn html_in_a_line_arrives_escaped() {
		let state = lazy_state();
		let id = JobId(8);
		let mut log = state.logs.open(id).await.unwrap();
		log.push(proto::Stream::Stdout, "<img src=x onerror=alert(1)>")
			.await;
		log.finish().await;

		let events = read(job_log(&state, Some(id), "log")).await;
		let html: String = events.iter().map(Sse::html).collect();
		assert!(!html.contains("<img"), "{html}");
		assert!(
			html.contains("&lt;img src=x onerror=alert(1)&gt;"),
			"{html}"
		);

		let agent = stream::iter([Ok(proto::Event::Log {
			stream: proto::Stream::Stderr,
			line: "<script>alert(1)</script>".to_owned(),
		})])
		.boxed();
		let events = read(container_log(agent, "log", CancellationToken::new())).await;
		let html: String = events.iter().map(Sse::html).collect();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("&lt;script&gt;"), "{html}");
	}

	#[tokio::test]
	async fn ten_thousand_fast_lines_make_few_events() {
		let events = read(container_log(
			agent_lines(10_000),
			"log",
			CancellationToken::new(),
		))
		.await;
		assert!(events.len() <= 300, "{} events", events.len());
		let seen = lines(&events);
		assert_eq!(seen.len(), 10_000);
		assert!(
			seen.iter()
				.enumerate()
				.all(|(i, l)| *l == format!("line {i}"))
		);
		assert!(ends_with_live_false(&events));
	}

	#[tokio::test]
	async fn a_slow_trickle_still_goes_out_without_waiting_for_the_end() {
		let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
		let events = tokio_stream_from(rx);
		let resp = container_log(events, "log", CancellationToken::new());
		let mut body = resp.into_body().into_data_stream();

		tx.send(Ok(proto::Event::Log {
			stream: proto::Stream::Stdout,
			line: "early".to_owned(),
		}))
		.unwrap();
		let first = tokio::time::timeout(Duration::from_secs(2), body.next())
			.await
			.expect("a line must not wait for the stream to end")
			.unwrap()
			.unwrap();
		assert!(String::from_utf8_lossy(&first).contains("early"));
	}

	fn tokio_stream_from(
		mut rx: tokio::sync::mpsc::UnboundedReceiver<Result<proto::Event, ServerError>>,
	) -> BoxStream<'static, Result<proto::Event, ServerError>> {
		stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed()
	}

	#[tokio::test]
	async fn a_long_replay_is_capped_with_a_notice() {
		let state = lazy_state();
		let id = JobId(9);
		let mut log = state.logs.open(id).await.unwrap();
		for i in 0..6000 {
			log.push(proto::Stream::Stdout, &format!("l{i}")).await;
		}
		log.finish().await;

		let events = read(job_log(&state, Some(id), "log")).await;
		assert!(events.len() <= 300, "{} events", events.len());
		let seen = lines(&events);
		assert_eq!(seen.len(), 5001);
		assert_eq!(seen[0], "l0");
		assert_eq!(seen[4999], "l4999");
		assert!(seen[5000].contains("truncated"), "{}", seen[5000]);
		assert!(ends_with_live_false(&events));
	}

	#[tokio::test]
	async fn lagging_becomes_a_notice_line() {
		let state = lazy_state();
		let id = JobId(10);
		let mut log = state.logs.open(id).await.unwrap();
		let resp = job_log(&state, Some(id), "log");
		// More lines than the channel holds, pushed before anyone reads.
		for i in 0..3000 {
			log.push(proto::Stream::Stdout, &format!("l{i}")).await;
		}
		log.finish().await;

		let events = read(resp).await;
		let seen = lines(&events);
		assert!(seen.iter().any(|l| l.contains("skipped")), "{seen:?}");
		assert!(ends_with_live_false(&events));
	}

	#[tokio::test]
	async fn a_broken_agent_stream_shows_a_notice_and_ends() {
		let agent = stream::iter([
			Ok(proto::Event::Log {
				stream: proto::Stream::Stdout,
				line: "hello".to_owned(),
			}),
			Err(ServerError::NotFound),
		])
		.boxed();
		let events = read(container_log(agent, "log", CancellationToken::new())).await;
		let seen = lines(&events);
		assert_eq!(seen[0], "hello");
		assert!(seen[1].contains("interrupted"), "{seen:?}");
		assert!(ends_with_live_false(&events));
	}

	#[tokio::test]
	async fn cancelling_shutdown_ends_the_stream() {
		let state = lazy_state();
		let id = JobId(11);
		let mut log = state.logs.open(id).await.unwrap();
		log.push(proto::Stream::Stdout, "running").await;

		let resp = job_log(&state, Some(id), "log");
		let reader = tokio::spawn(read(resp));
		tokio::time::sleep(Duration::from_millis(200)).await;
		assert!(!reader.is_finished(), "the job is still running");
		state.shutdown.cancel();

		let events = tokio::time::timeout(Duration::from_secs(2), reader)
			.await
			.expect("stream must end on shutdown")
			.unwrap();
		assert_eq!(lines(&events), ["running"]);
		assert!(ends_with_live_false(&events));
		drop(log);
	}

	#[tokio::test]
	async fn the_test_route_serves_the_view_and_the_stream() {
		let state = lazy_state();
		let id = JobId(12);
		let mut log = state.logs.open(id).await.unwrap();
		log.push(proto::Stream::Stdout, "hi").await;
		log.finish().await;
		let app = test_router().with_state(state);

		let get = |uri: &'static str| {
			let app = app.clone();
			async move {
				let resp = app
					.oneshot(Request::get(uri).body(Body::empty()).unwrap())
					.await
					.unwrap();
				assert!(resp.status().is_success());
				resp
			}
		};
		let page = get("/_test/live/view").await;
		let bytes = axum::body::to_bytes(page.into_body(), usize::MAX)
			.await
			.unwrap();
		let page = String::from_utf8(bytes.to_vec()).unwrap();
		assert!(
			page.contains(r#"id="log""#) && page.contains("data-init=\"@get("),
			"{page}"
		);
		assert!(page.contains("data-on-interval__duration.3s"), "{page}");

		let stream = get("/_test/live/job/12").await;
		assert_eq!(stream.headers()[header::CONTENT_TYPE], "text/event-stream");
		let events = read(stream).await;
		assert_eq!(lines(&events), ["hi"]);
	}

	#[test]
	fn log_view_embeds_the_stream_url_safely() {
		let html = log_view("log", "/x');alert(1);//").0;
		assert!(html.contains(r#"id="log""#), "{html}");
		assert!(html.contains(r"/x\u0027);alert(1);//')"), "{html}");
		assert!(html.contains("data-bind:_follow"), "{html}");
	}
}
