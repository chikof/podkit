//! The copy loop of one terminal session: bytes and control messages between the browser and
//! the agent, the idle timeout, the optional transcript.
//!
//! It knows nothing about axum or tungstenite: each side is a stream of [`Frame`]s and a
//! sink of them, so the tests drive it with plain channels and a paused clock.
//!
//! Frames: **binary** is terminal bytes, **text** is JSON. From the browser the only text
//! that is acted on is [`proto::TerminalControl`] (a resize, clamped here, then forwarded);
//! anything else is dropped, so the browser cannot talk to the agent beyond keystrokes and
//! window size. From the agent, text is a [`proto::TerminalNotice`] and goes to the browser.

use std::time::Duration;

use bytes::Bytes;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use proto::{TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, TerminalControl, TerminalNotice};
use tokio::time::{Instant, sleep_until};

/// How long a terminal may sit with nothing typed and nothing printed.
pub const IDLE_TIMEOUT: Duration = Duration::from_mins(15);

/// The most output a recorded session keeps.
pub const MAX_TRANSCRIPT: usize = 1024 * 1024;

/// One WebSocket message, reduced to what the loop cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
	Binary(Bytes),
	Text(String),
	/// The peer closed or the connection broke. Sent to ask the peer to close, too.
	Close,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
	/// The browser went away.
	Closed,
	/// The shell ended (`exit`, or the container stopped).
	Exited,
	/// Nothing moved for [`Options::idle`].
	Idle,
	/// The agent reported a failure or the connection to it broke.
	Error,
}

impl EndReason {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Closed => "closed",
			Self::Exited => "exited",
			Self::Idle => "idle",
			Self::Error => "error",
		}
	}
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
	pub idle: Duration,
	/// Keep the output, up to this many bytes.
	pub record: Option<usize>,
}

/// What a finished session leaves behind.
#[derive(Debug)]
pub struct Outcome {
	pub reason: EndReason,
	pub exit_code: Option<i64>,
	/// Bytes the container printed.
	pub bytes_out: u64,
	/// Bytes the user typed. Counted, never kept.
	pub bytes_in: u64,
	pub resizes: u32,
	pub transcript: Option<Vec<u8>>,
	pub truncated: bool,
}

fn notice(n: &TerminalNotice) -> Frame {
	Frame::Text(serde_json::to_string(n).unwrap_or_default())
}

/// The size a browser asked for, kept inside the window sizes the agent accepts.
fn clamp(cols: u16, rows: u16) -> (u16, u16) {
	(
		cols.clamp(1, TERMINAL_MAX_COLS),
		rows.clamp(1, TERMINAL_MAX_ROWS),
	)
}

/// Runs the session until one side ends or the idle timeout fires.
pub async fn pump<BR, BT, AR, AT>(
	mut browser_rx: BR,
	mut browser_tx: BT,
	mut agent_rx: AR,
	mut agent_tx: AT,
	options: Options,
) -> Outcome
where
	BR: Stream<Item = Frame> + Unpin,
	BT: Sink<Frame> + Unpin,
	AR: Stream<Item = Frame> + Unpin,
	AT: Sink<Frame> + Unpin,
{
	let mut out = Outcome {
		reason: EndReason::Closed,
		exit_code: None,
		bytes_out: 0,
		bytes_in: 0,
		resizes: 0,
		transcript: options.record.map(|_| Vec::new()),
		truncated: false,
	};
	let idle = sleep_until(Instant::now() + options.idle);
	tokio::pin!(idle);

	loop {
		tokio::select! {
			biased;
			frame = browser_rx.next() => match frame {
				Some(Frame::Binary(bytes)) => {
					out.bytes_in += bytes.len() as u64;
					idle.as_mut().reset(Instant::now() + options.idle);
					if agent_tx.send(Frame::Binary(bytes)).await.is_err() {
						out.reason = EndReason::Error;
						break;
					}
				}
				Some(Frame::Text(text)) => {
					if let Ok(TerminalControl::Resize { cols, rows }) = serde_json::from_str(&text) {
						let (cols, rows) = clamp(cols, rows);
						out.resizes += 1;
						let msg = serde_json::to_string(&TerminalControl::Resize { cols, rows })
							.unwrap_or_default();
						if agent_tx.send(Frame::Text(msg)).await.is_err() {
							out.reason = EndReason::Error;
							break;
						}
					}
				}
				Some(Frame::Close) | None => {
					out.reason = EndReason::Closed;
					break;
				}
			},
			frame = agent_rx.next() => match frame {
				Some(Frame::Binary(bytes)) => {
					out.bytes_out += bytes.len() as u64;
					idle.as_mut().reset(Instant::now() + options.idle);
					if let (Some(limit), Some(log)) = (options.record, out.transcript.as_mut()) {
						let room = limit.saturating_sub(log.len());
						log.extend_from_slice(&bytes[..bytes.len().min(room)]);
						out.truncated |= bytes.len() > room;
					}
					if browser_tx.send(Frame::Binary(bytes)).await.is_err() {
						out.reason = EndReason::Closed;
						break;
					}
				}
				Some(Frame::Text(text)) => {
					match serde_json::from_str::<TerminalNotice>(&text) {
						Ok(TerminalNotice::Exit { code }) => {
							out.exit_code = code;
							out.reason = EndReason::Exited;
							let _ = browser_tx.send(Frame::Text(text)).await;
							break;
						}
						Ok(TerminalNotice::Error { .. }) => {
							out.reason = EndReason::Error;
							let _ = browser_tx.send(Frame::Text(text)).await;
							break;
						}
						Err(_) => {}
					}
				}
				Some(Frame::Close) | None => {
					out.reason = EndReason::Exited;
					break;
				}
			},
			() = &mut idle => {
				out.reason = EndReason::Idle;
				let minutes = options.idle.as_secs() / 60;
				let message = if minutes >= 1 {
					format!("Closed after {minutes} minutes with nothing typed or printed.")
				} else {
					"Closed after being idle.".to_owned()
				};
				let _ = browser_tx.send(notice(&TerminalNotice::Error { message })).await;
				break;
			}
		}
	}
	// Ask both sides to wrap up. Dropping the agent socket closes the shell's stdin.
	let _ = browser_tx.send(Frame::Close).await;
	let _ = agent_tx.send(Frame::Close).await;
	out
}

#[cfg(test)]
mod tests {
	use futures_util::sink;
	use futures_util::stream;
	use tokio::sync::mpsc;

	use super::*;

	type Rx = stream::BoxStream<'static, Frame>;
	type Tx = std::pin::Pin<Box<dyn Sink<Frame, Error = ()> + Send>>;

	/// A stream fed by `.0` and a sink that lands in `.1`'s receiver.
	struct Side {
		feed: mpsc::UnboundedSender<Frame>,
		seen: mpsc::UnboundedReceiver<Frame>,
		rx: Rx,
		tx: Tx,
	}

	fn side() -> Side {
		let (feed, inbox) = mpsc::unbounded_channel();
		let (out, seen) = mpsc::unbounded_channel();
		let rx =
			stream::unfold(inbox, |mut r| async move { r.recv().await.map(|f| (f, r)) }).boxed();
		let tx = Box::pin(sink::unfold(out, |out, f: Frame| async move {
			let _ = out.send(f);
			Ok::<_, ()>(out)
		}));
		Side { feed, seen, rx, tx }
	}

	fn options(idle_secs: u64, record: Option<usize>) -> Options {
		Options {
			idle: Duration::from_secs(idle_secs),
			record,
		}
	}

	fn drain(rx: &mut mpsc::UnboundedReceiver<Frame>) -> Vec<Frame> {
		let mut all = Vec::new();
		while let Ok(f) = rx.try_recv() {
			all.push(f);
		}
		all
	}

	fn resize_text(cols: u16, rows: u16) -> Frame {
		Frame::Text(serde_json::to_string(&TerminalControl::Resize { cols, rows }).unwrap())
	}

	#[tokio::test(start_paused = true)]
	async fn keystrokes_go_to_the_agent_and_output_comes_back() {
		let (mut b, mut a) = (side(), side());
		b.feed
			.send(Frame::Binary(Bytes::from_static(b"echo hi\n")))
			.unwrap();
		a.feed
			.send(Frame::Binary(Bytes::from_static(b"hi\r\n")))
			.unwrap();
		a.feed
			.send(notice(&TerminalNotice::Exit { code: Some(0) }))
			.unwrap();
		let out = pump(b.rx, b.tx, a.rx, a.tx, options(900, None)).await;
		assert_eq!(out.reason, EndReason::Exited);
		assert_eq!(out.exit_code, Some(0));
		assert_eq!((out.bytes_in, out.bytes_out), (8, 4));
		assert!(
			out.transcript.is_none(),
			"recording is off unless asked for"
		);
		assert!(drain(&mut a.seen).contains(&Frame::Binary(Bytes::from_static(b"echo hi\n"))));
		let to_browser = drain(&mut b.seen);
		assert!(to_browser.contains(&Frame::Binary(Bytes::from_static(b"hi\r\n"))));
	}

	#[tokio::test(start_paused = true)]
	async fn resize_is_clamped_and_forwarded_and_other_text_is_dropped() {
		let (b, mut a) = (side(), side());
		b.feed.send(resize_text(120, 40)).unwrap();
		b.feed.send(resize_text(0, 60000)).unwrap();
		b.feed
			.send(Frame::Text(
				r#"{"type":"exec","cmd":"rm -rf /"}"#.to_owned(),
			))
			.unwrap();
		b.feed.send(Frame::Text("not json".to_owned())).unwrap();
		b.feed.send(Frame::Close).unwrap();
		let out = pump(b.rx, b.tx, a.rx, a.tx, options(900, None)).await;
		assert_eq!(out.reason, EndReason::Closed);
		assert_eq!(out.resizes, 2);
		let to_agent = drain(&mut a.seen);
		assert_eq!(
			to_agent,
			[resize_text(120, 40), resize_text(1, 200), Frame::Close],
			"only well-formed resizes reach the agent, inside its limits"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn idle_session_is_closed_after_the_timeout() {
		let (mut b, a) = (side(), side());
		let started = Instant::now();
		let out = pump(b.rx, b.tx, a.rx, a.tx, options(15 * 60, None)).await;
		assert_eq!(out.reason, EndReason::Idle);
		assert_eq!(started.elapsed(), Duration::from_mins(15));
		let to_browser = drain(&mut b.seen);
		assert!(
			matches!(&to_browser[0], Frame::Text(t) if t.contains("15 minutes")),
			"the browser is told why: {to_browser:?}"
		);
		assert_eq!(to_browser.last(), Some(&Frame::Close));
		drop((b.feed, a.feed));
	}

	#[tokio::test(start_paused = true)]
	async fn activity_in_either_direction_postpones_the_timeout() {
		let (b, a) = (side(), side());
		let (bf, af) = (b.feed.clone(), a.feed.clone());
		let started = Instant::now();
		let driver = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_secs(50)).await;
			bf.send(Frame::Binary(Bytes::from_static(b"x"))).unwrap();
			tokio::time::sleep(Duration::from_secs(50)).await;
			af.send(Frame::Binary(Bytes::from_static(b"y"))).unwrap();
		});
		let out = pump(b.rx, b.tx, a.rx, a.tx, options(60, None)).await;
		driver.await.unwrap();
		assert_eq!(out.reason, EndReason::Idle);
		assert_eq!(
			started.elapsed(),
			Duration::from_secs(160),
			"60 s after the last of the two events"
		);
		drop((b.feed, a.feed));
	}

	#[tokio::test(start_paused = true)]
	async fn transcript_keeps_output_only_and_stops_at_the_cap() {
		let (b, a) = (side(), side());
		b.feed
			.send(Frame::Binary(Bytes::from_static(b"secret-password\n")))
			.unwrap();
		a.feed
			.send(Frame::Binary(Bytes::from_static(b"hello ")))
			.unwrap();
		a.feed
			.send(Frame::Binary(Bytes::from_static(b"world")))
			.unwrap();
		a.feed.send(Frame::Close).unwrap();
		let out = pump(b.rx, b.tx, a.rx, a.tx, options(900, Some(8))).await;
		assert_eq!(out.transcript.as_deref(), Some(&b"hello wo"[..]));
		assert!(out.truncated);
		assert_eq!(out.reason, EndReason::Exited);
		assert!(
			!String::from_utf8_lossy(out.transcript.as_deref().unwrap()).contains("password"),
			"keystrokes are never recorded"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn an_agent_error_ends_the_session_and_reaches_the_browser() {
		let (mut b, a) = (side(), side());
		a.feed
			.send(notice(&TerminalNotice::Error {
				message: "the shell could not start".to_owned(),
			}))
			.unwrap();
		let out = pump(b.rx, b.tx, a.rx, a.tx, options(900, None)).await;
		assert_eq!(out.reason, EndReason::Error);
		assert!(matches!(&drain(&mut b.seen)[0], Frame::Text(t) if t.contains("could not start")));
		drop((b.feed, a.feed));
	}
}
