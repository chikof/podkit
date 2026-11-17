//! Per-job logs: an append-only NDJSON file per job plus a broadcast channel
//! for live followers, served to clients over SSE.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, Stream, StreamExt};
use proto::Stream as LogStream;
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, Lines};
use tokio::sync::broadcast;

use crate::ids::JobId;
use crate::state::AppState;

const MAX_LINE_BYTES: usize = 8 * 1024;
const MAX_JOB_LOG_BYTES: u64 = 10 << 20;
const BROADCAST_CAPACITY: usize = 1024;

type Live = Arc<Mutex<HashMap<JobId, broadcast::Sender<LogLine>>>>;

/// One log record. `seq` starts at 1 and is unique and increasing per job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
	pub seq: u64,
	/// Unix milliseconds.
	pub ts: i64,
	pub stream: LogStream,
	pub line: String,
}

/// Owns the log directory and the live channel of every running job.
pub struct LogHub {
	dir: PathBuf,
	live: Live,
}

impl LogHub {
	pub fn new(dir: PathBuf) -> Self {
		Self {
			dir,
			live: Live::default(),
		}
	}

	/// Creates the job's log file and its live channel.
	pub async fn open(&self, id: JobId) -> std::io::Result<JobLog> {
		let file = File::create(self.path(id)).await?;
		let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
		self.live
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(id, tx.clone());
		Ok(JobLog {
			id,
			live: Arc::clone(&self.live),
			tx,
			writer: BufWriter::new(file),
			seq: 0,
			bytes: 0,
			truncated: false,
			io_failed: false,
		})
	}

	/// A receiver of lines pushed from now on, or `None` if the job has no open log.
	pub fn subscribe(&self, id: JobId) -> Option<broadcast::Receiver<LogLine>> {
		self.live
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.get(&id)
			.map(broadcast::Sender::subscribe)
	}

	pub fn path(&self, id: JobId) -> PathBuf {
		self.dir.join(format!("{id}.ndjson"))
	}
}

/// Write handle of one running job's log.
pub struct JobLog {
	id: JobId,
	live: Live,
	tx: broadcast::Sender<LogLine>,
	writer: BufWriter<File>,
	seq: u64,
	bytes: u64,
	truncated: bool,
	io_failed: bool,
}

impl JobLog {
	/// Appends a line to the file and to live followers. Write errors are
	/// logged once and never returned: a log failure must not fail the job.
	pub async fn push(&mut self, stream: LogStream, line: &str) {
		if self.truncated {
			return;
		}
		let mut record = self.record(stream, clip(line));
		let mut json = encode(&record);
		if self.bytes + json.len() as u64 > MAX_JOB_LOG_BYTES {
			self.truncated = true;
			record = self.record(LogStream::System, "log truncated at 10 MiB");
			json = encode(&record);
		}
		self.bytes += json.len() as u64;
		self.write(&json).await;
		// No receivers is the normal case when nobody follows the job.
		let _ = self.tx.send(record);
	}

	/// Flushes and closes the live channel, which ends every follower's stream.
	pub async fn finish(mut self) {
		if let Err(e) = self.writer.flush().await {
			tracing::warn!(job = %self.id, "failed to flush job log: {e}");
		}
	}

	fn record(&mut self, stream: LogStream, line: &str) -> LogLine {
		self.seq += 1;
		LogLine {
			seq: self.seq,
			ts: now_ms(),
			stream,
			line: line.to_owned(),
		}
	}

	async fn write(&mut self, json: &[u8]) {
		if self.io_failed {
			return;
		}
		// Flushed per line: a follower that subscribes later replays the file, so
		// a buffered line would be missing from both the replay and the channel.
		let result = async {
			self.writer.write_all(json).await?;
			self.writer.flush().await
		}
		.await;
		if let Err(e) = result {
			self.io_failed = true;
			tracing::warn!(job = %self.id, "job log write failed, further lines are live-only: {e}");
		}
	}
}

impl Drop for JobLog {
	fn drop(&mut self) {
		self.live
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.remove(&self.id);
	}
}

fn encode(record: &LogLine) -> Vec<u8> {
	// Plain strings and integers only, so serialization cannot fail.
	let mut json = serde_json::to_vec(record).unwrap_or_default();
	json.push(b'\n');
	json
}

fn clip(line: &str) -> &str {
	if line.len() <= MAX_LINE_BYTES {
		return line;
	}
	let mut end = MAX_LINE_BYTES;
	while !line.is_char_boundary(end) {
		end -= 1;
	}
	&line[..end]
}

fn now_ms() -> i64 {
	i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

/// One step of a followed log.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
	Line(LogLine),
	Lagged(u64),
	End,
}

struct Follow {
	path: Option<PathBuf>,
	lines: Option<Lines<BufReader<File>>>,
	rx: Option<broadcast::Receiver<LogLine>>,
	/// Highest seq yielded so far; live lines at or below it were already replayed.
	max: u64,
	done: bool,
}

impl Follow {
	/// The next frame and whether it came from the stored file (a replay)
	/// rather than the live channel.
	async fn next(&mut self) -> Option<(Frame, bool)> {
		if self.done {
			return None;
		}
		if let Some(path) = self.path.take() {
			self.lines = File::open(path)
				.await
				.ok()
				.map(|f| BufReader::new(f).lines());
		}
		while let Some(lines) = &mut self.lines {
			match lines.next_line().await {
				Ok(Some(raw)) => {
					// A partially written trailing line fails to parse; the live
					// channel delivers it in full.
					if let Ok(line) = serde_json::from_str::<LogLine>(&raw) {
						self.max = self.max.max(line.seq);
						return Some((Frame::Line(line), true));
					}
				}
				Ok(None) | Err(_) => self.lines = None,
			}
		}
		let Some(rx) = &mut self.rx else {
			self.done = true;
			return Some((Frame::End, false));
		};
		loop {
			match rx.recv().await {
				Ok(line) if line.seq > self.max => {
					self.max = line.seq;
					return Some((Frame::Line(line), false));
				}
				Ok(_) => {}
				Err(broadcast::error::RecvError::Lagged(n)) => {
					return Some((Frame::Lagged(n), false));
				}
				Err(broadcast::error::RecvError::Closed) => {
					self.done = true;
					return Some((Frame::End, false));
				}
			}
		}
	}
}

/// Replays `path`, then follows `rx`, yielding every line once in seq order.
/// `rx` must have been subscribed before the file is read.
pub(crate) fn frames(
	path: Option<PathBuf>,
	rx: Option<broadcast::Receiver<LogLine>>,
) -> impl Stream<Item = Frame> + Send + 'static {
	tagged_frames(path, rx).map(|(frame, _)| frame)
}

/// [`frames`], each paired with whether it was replayed from the stored file.
pub(crate) fn tagged_frames(
	path: Option<PathBuf>,
	rx: Option<broadcast::Receiver<LogLine>>,
) -> impl Stream<Item = (Frame, bool)> + Send + 'static {
	let state = Follow {
		path,
		lines: None,
		rx,
		max: 0,
		done: false,
	};
	stream::unfold(state, |mut s| async move {
		let frame = s.next().await?;
		Some((frame, s))
	})
}

fn to_event(frame: Frame) -> Event {
	match frame {
		Frame::Line(line) => Event::default()
			.event("log")
			.data(serde_json::to_string(&line).unwrap_or_default()),
		Frame::Lagged(n) => Event::default().event("lagged").data(n.to_string()),
		Frame::End => Event::default().event("end").data(""),
	}
}

/// Streams a job's log as SSE: stored lines first, then live ones until the
/// job finishes. `None` (no job yet) ends immediately.
pub fn job_sse(
	state: &AppState,
	job: Option<JobId>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send + 'static + use<>> {
	// Subscribing before the replay opens the file is what makes the merge lossless.
	let (path, rx) = match job {
		Some(id) => (Some(state.logs.path(id)), state.logs.subscribe(id)),
		None => (None, None),
	};
	let shutdown = state.shutdown.clone();
	let events = stream::unfold(Some(Box::pin(frames(path, rx))), move |frames| {
		let shutdown = shutdown.clone();
		async move {
			let mut frames = frames?;
			let frame = tokio::select! {
				biased;
				() = shutdown.cancelled() => {
					return Some((Ok(to_event(Frame::End)), None));
				}
				frame = frames.next() => frame?,
			};
			Some((Ok(to_event(frame)), Some(frames)))
		}
	});
	Sse::new(events).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
	use super::*;

	async fn collect(path: PathBuf, rx: Option<broadcast::Receiver<LogLine>>) -> Vec<Frame> {
		frames(Some(path), rx).collect().await
	}

	#[tokio::test]
	async fn lines_before_and_after_subscribe_arrive_once_in_order() {
		let dir = tempfile::tempdir().unwrap();
		let hub = LogHub::new(dir.path().to_owned());
		let id = JobId(7);
		let mut log = hub.open(id).await.unwrap();

		log.push(LogStream::Stdout, "one").await;
		log.push(LogStream::Stderr, "two").await;
		let rx = hub.subscribe(id);
		assert!(rx.is_some());
		log.push(LogStream::System, "three").await;
		log.push(LogStream::Stdout, "four").await;
		log.finish().await;

		let frames = collect(hub.path(id), rx).await;
		let seen: Vec<(u64, &str)> = frames
			.iter()
			.filter_map(|f| match f {
				Frame::Line(l) => Some((l.seq, l.line.as_str())),
				_ => None,
			})
			.collect();
		assert_eq!(seen, [(1, "one"), (2, "two"), (3, "three"), (4, "four")]);
		assert_eq!(frames.last(), Some(&Frame::End));
		assert_eq!(frames.len(), 5);
		assert!(hub.subscribe(id).is_none());
	}

	#[tokio::test]
	async fn live_lines_after_replay_are_forwarded() {
		let dir = tempfile::tempdir().unwrap();
		let hub = LogHub::new(dir.path().to_owned());
		let id = JobId(8);
		let mut log = hub.open(id).await.unwrap();
		log.push(LogStream::Stdout, "stored").await;
		let rx = hub.subscribe(id);

		let follower = tokio::spawn(collect(hub.path(id), rx));
		tokio::task::yield_now().await;
		log.push(LogStream::Stdout, "live").await;
		log.finish().await;

		let frames = follower.await.unwrap();
		let seqs: Vec<u64> = frames
			.iter()
			.filter_map(|f| match f {
				Frame::Line(l) => Some(l.seq),
				_ => None,
			})
			.collect();
		assert_eq!(seqs, [1, 2]);
		assert_eq!(frames.last(), Some(&Frame::End));
	}

	#[tokio::test]
	async fn log_is_truncated_once_past_the_cap() {
		let dir = tempfile::tempdir().unwrap();
		let hub = LogHub::new(dir.path().to_owned());
		let id = JobId(9);
		let mut log = hub.open(id).await.unwrap();
		let big = "x".repeat(MAX_LINE_BYTES * 2);
		for _ in 0..(MAX_JOB_LOG_BYTES / MAX_LINE_BYTES as u64 + 10) {
			log.push(LogStream::Stdout, &big).await;
		}
		log.finish().await;

		let frames = collect(hub.path(id), None).await;
		let lines: Vec<&LogLine> = frames
			.iter()
			.filter_map(|f| match f {
				Frame::Line(l) => Some(l),
				_ => None,
			})
			.collect();
		let last = lines.last().unwrap();
		assert_eq!(last.stream, LogStream::System);
		assert_eq!(last.line, "log truncated at 10 MiB");
		assert_eq!(lines[0].line.len(), MAX_LINE_BYTES);
	}
}
