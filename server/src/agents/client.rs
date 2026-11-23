use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::{Request, Response, client::conn::http1};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::UnixStream;

use super::tunnel::SshTunnel;
use crate::error::ServerError;

const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// `df` and `nvidia-smi` may each take a few seconds on a busy host.
const HARDWARE_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A scan exports the image and runs two containers; the biggest images take minutes.
const SCAN_TIMEOUT: Duration = Duration::from_mins(25);
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Talks to one server's agent over its unix socket.
#[derive(Clone)]
pub struct AgentClient {
	socket: PathBuf,
	/// Keeps a remote tunnel (and its socket file) alive while requests and
	/// streams made through this client are in flight.
	tunnel: Option<Arc<SshTunnel>>,
}

impl AgentClient {
	pub(super) fn new(socket: PathBuf, tunnel: Option<Arc<SshTunnel>>) -> Self {
		Self { socket, tunnel }
	}

	/// # Errors
	/// `AgentUnavailable` if the agent cannot be reached or answers badly.
	pub async fn health(&self) -> Result<proto::Health, ServerError> {
		self.json_within(HEALTH_TIMEOUT, Method::GET, "/v1/health", None::<&()>)
			.await
	}

	/// # Errors
	/// See [`AgentClient::health`].
	pub async fn hardware(&self) -> Result<proto::Hardware, ServerError> {
		self.json_within(HARDWARE_TIMEOUT, Method::GET, "/v1/hardware", None::<&()>)
			.await
	}

	/// One live sample of the host and its containers.
	///
	/// # Errors
	/// See [`AgentClient::health`].
	pub async fn stats(&self) -> Result<proto::Stats, ServerError> {
		self.json_within(HARDWARE_TIMEOUT, Method::GET, "/v1/stats", None::<&()>)
			.await
	}

	/// Restarts, OOM kills and crashes of podkit containers in `since..until` (unix seconds),
	/// from Podman's event log.
	///
	/// # Errors
	/// See [`AgentClient::health`].
	pub async fn events(
		&self,
		since: i64,
		until: i64,
	) -> Result<Vec<proto::ContainerEvent>, ServerError> {
		self.json_within(
			HARDWARE_TIMEOUT,
			Method::GET,
			&format!("/v1/events?since={since}&until={until}"),
			None::<&()>,
		)
		.await
	}

	/// Connections, size and activity of the database containers in `targets`. A database
	/// that does not answer is missing from the reply.
	///
	/// # Errors
	/// See [`AgentClient::health`].
	pub async fn db_stats(
		&self,
		targets: Vec<proto::DbStatsTarget>,
	) -> Result<Vec<proto::DbStat>, ServerError> {
		self.json_within(
			HARDWARE_TIMEOUT,
			Method::POST,
			"/v1/db-stats",
			Some(&proto::DbStatsRequest { targets }),
		)
		.await
	}

	/// Searches the container logs the agent keeps.
	///
	/// # Errors
	/// `Validation` carrying the agent's message for a query it refuses (a regular expression
	/// that does not compile); see [`AgentClient::health`] otherwise.
	pub async fn log_search(
		&self,
		req: &proto::LogSearchRequest,
	) -> Result<proto::LogSearchResult, ServerError> {
		self.json_within_messages(REQUEST_TIMEOUT, Method::POST, "/v1/logs/search", Some(req))
			.await
	}

	/// Tells the agent how long and how much to keep, and where to forward.
	///
	/// # Errors
	/// See [`AgentClient::json`].
	pub async fn log_config(&self, config: &proto::LogConfig) -> Result<(), ServerError> {
		self.send_json(Method::PUT, "/v1/logs/config", config).await
	}

	/// What the agent's log store holds and how forwarding is doing.
	///
	/// # Errors
	/// See [`AgentClient::health`].
	pub async fn log_status(&self) -> Result<proto::LogStatus, ServerError> {
		self.json_within(HEALTH_TIMEOUT, Method::GET, "/v1/logs/status", None::<&()>)
			.await
	}

	/// Which image id a reference names on the server.
	///
	/// # Errors
	/// `Validation` carrying the agent's message when the image is missing;
	/// see [`AgentClient::health`] otherwise.
	pub async fn scan_resolve(&self, image: &str) -> Result<String, ServerError> {
		let body = proto::ScanResolveRequest {
			image: image.to_owned(),
		};
		let resp: proto::ScanResolveResponse = self
			.json_within_messages(
				REQUEST_TIMEOUT,
				Method::POST,
				"/v1/scan/resolve",
				Some(&body),
			)
			.await?;
		Ok(resp.image_id)
	}

	/// Runs a scan (or a rescan from an SBOM). Takes minutes for a big image.
	///
	/// # Errors
	/// `Validation` carrying the agent's message when the scan fails (an
	/// out-of-memory scanner says so); see [`AgentClient::health`] otherwise.
	pub async fn scan(&self, req: &proto::ScanRequest) -> Result<proto::ScanResult, ServerError> {
		self.json_within_messages(SCAN_TIMEOUT, Method::POST, "/v1/scan", Some(req))
			.await
	}

	/// Like `json_within`, but a non-2xx answer keeps the agent's message.
	async fn json_within_messages<T: DeserializeOwned>(
		&self,
		limit: Duration,
		method: Method,
		path: &str,
		body: Option<&(impl Serialize + Sync)>,
	) -> Result<T, ServerError> {
		let body = encode(body)?;
		let request = self.request(method, path, body);
		with_timeout(limit, async {
			let resp = request?.await?;
			let status = resp.status();
			let bytes = resp
				.into_body()
				.collect()
				.await
				.map_err(unavailable("read agent response"))?
				.to_bytes();
			if status.is_success() {
				return serde_json::from_slice(&bytes)
					.map_err(unavailable("decode agent response"));
			}
			let message = serde_json::from_slice::<proto::ErrorBody>(&bytes)
				.ok()
				.map(|b| b.error);
			if let Some(message) = message {
				Err(ServerError::Validation(message))
			} else {
				tracing::warn!(%status, "agent returned an error");
				Err(ServerError::AgentUnavailable)
			}
		})
		.await
	}

	/// # Errors
	/// See [`AgentClient::health`]; 409 maps to `Conflict`, 404 to `Validation`.
	pub async fn json<T: DeserializeOwned>(
		&self,
		method: Method,
		path: &str,
		body: Option<&(impl Serialize + Sync)>,
	) -> Result<T, ServerError> {
		self.json_within(REQUEST_TIMEOUT, method, path, body).await
	}

	/// Expects a 2xx response and ignores its body.
	///
	/// # Errors
	/// See [`AgentClient::json`].
	pub async fn send(&self, method: Method, path: &str) -> Result<(), ServerError> {
		let request = self.request(method, path, None);
		with_timeout(REQUEST_TIMEOUT, async {
			let resp = ok_or_error(request?.await?).await?;
			drain(resp).await
		})
		.await
	}

	/// Like [`AgentClient::send`], with a JSON request body.
	///
	/// # Errors
	/// See [`AgentClient::json`].
	pub async fn send_json(
		&self,
		method: Method,
		path: &str,
		body: &(impl Serialize + Sync),
	) -> Result<(), ServerError> {
		let body = encode(Some(body))?;
		let request = self.request(method, path, body);
		with_timeout(REQUEST_TIMEOUT, async {
			let resp = ok_or_error(request?.await?).await?;
			drain(resp).await
		})
		.await
	}

	/// Starts an NDJSON stream. There is no overall timeout: callers cancel by
	/// dropping the stream, which closes the connection.
	///
	/// # Errors
	/// Fails before the first event if the request is rejected; later failures
	/// are yielded as stream items.
	pub async fn stream(
		&self,
		method: Method,
		path: &str,
		body: Option<&(impl Serialize + Sync)>,
	) -> Result<BoxStream<'static, Result<proto::Event, ServerError>>, ServerError> {
		let body = encode(body)?;
		let resp = ok_or_error(self.request(method, path, body)?.await?).await?;
		let state = StreamState {
			body: resp.into_body(),
			lines: LineSplitter::new(MAX_LINE_BYTES),
			done: false,
			_tunnel: self.tunnel.clone(),
		};
		Ok(futures_util::stream::unfold(state, next_event).boxed())
	}

	async fn json_within<T: DeserializeOwned>(
		&self,
		limit: Duration,
		method: Method,
		path: &str,
		body: Option<&(impl Serialize + Sync)>,
	) -> Result<T, ServerError> {
		let body = encode(body)?;
		let request = self.request(method, path, body);
		with_timeout(limit, async {
			let resp = ok_or_error(request?.await?).await?;
			let bytes = resp
				.into_body()
				.collect()
				.await
				.map_err(unavailable("read agent response"))?
				.to_bytes();
			serde_json::from_slice(&bytes).map_err(unavailable("decode agent response"))
		})
		.await
	}

	/// Connects and returns a future for the response head. Connecting and
	/// building the request happen here so the returned future owns no borrow
	/// of `self`.
	fn request(
		&self,
		method: Method,
		path: &str,
		body: Option<Bytes>,
	) -> Result<impl Future<Output = Result<Response<Incoming>, ServerError>> + use<>, ServerError>
	{
		let mut builder = Request::builder()
			.method(method)
			.uri(path)
			.header(HOST, "agent");
		if body.is_some() {
			builder = builder.header(CONTENT_TYPE, "application/json");
		}
		let request = builder
			.body(Full::new(body.unwrap_or_default()))
			.map_err(|e| {
				tracing::error!(error = %e, path, "invalid agent request");
				ServerError::Internal
			})?;
		let socket = self.socket.clone();
		Ok(async move {
			let stream = UnixStream::connect(&socket)
				.await
				.map_err(unavailable("connect to agent"))?;
			let (mut sender, conn) = http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
				.await
				.map_err(unavailable("agent handshake"))?;
			tokio::spawn(async move {
				if let Err(e) = conn.await {
					tracing::debug!(error = %e, "agent connection closed with error");
				}
			});
			sender
				.send_request(request)
				.await
				.map_err(unavailable("agent request"))
		})
	}
}

fn encode(body: Option<&impl Serialize>) -> Result<Option<Bytes>, ServerError> {
	body.map(|b| {
		serde_json::to_vec(b).map(Bytes::from).map_err(|e| {
			tracing::error!(error = %e, "failed to encode agent request body");
			ServerError::Internal
		})
	})
	.transpose()
}

async fn with_timeout<T>(
	limit: Duration,
	fut: impl Future<Output = Result<T, ServerError>>,
) -> Result<T, ServerError> {
	tokio::time::timeout(limit, fut).await.unwrap_or_else(|_| {
		tracing::warn!(?limit, "agent request timed out");
		Err(ServerError::AgentUnavailable)
	})
}

fn unavailable<E: Display>(what: &'static str) -> impl FnOnce(E) -> ServerError {
	move |e| {
		tracing::warn!(error = %e, "{what} failed");
		ServerError::AgentUnavailable
	}
}

async fn drain(resp: Response<Incoming>) -> Result<(), ServerError> {
	resp.into_body()
		.collect()
		.await
		.map(drop)
		.map_err(unavailable("read agent response"))
}

/// Passes 2xx responses through; maps everything else per the agent error contract.
async fn ok_or_error(resp: Response<Incoming>) -> Result<Response<Incoming>, ServerError> {
	let status = resp.status();
	if status.is_success() {
		return Ok(resp);
	}
	let bytes = Limited::new(resp.into_body(), MAX_ERROR_BODY_BYTES)
		.collect()
		.await
		.map(http_body_util::Collected::to_bytes)
		.unwrap_or_default();
	let message = serde_json::from_slice::<proto::ErrorBody>(&bytes)
		.ok()
		.map(|b| b.error);
	Err(match (status, message) {
		(StatusCode::CONFLICT, Some(message)) => ServerError::Conflict(message),
		(StatusCode::NOT_FOUND, Some(message)) => ServerError::Validation(message),
		_ => {
			tracing::warn!(%status, body = %String::from_utf8_lossy(&bytes), "agent returned an error");
			ServerError::AgentUnavailable
		}
	})
}

struct StreamState {
	body: Incoming,
	lines: LineSplitter,
	done: bool,
	_tunnel: Option<Arc<SshTunnel>>,
}

async fn next_event(
	mut st: StreamState,
) -> Option<(Result<proto::Event, ServerError>, StreamState)> {
	loop {
		if st.done {
			return None;
		}
		match st.lines.next_line() {
			Ok(Some(line)) => {
				let event = parse_event(&line);
				st.done = event.is_err();
				return Some((event, st));
			}
			Ok(None) => {}
			Err(e) => {
				st.done = true;
				return Some((Err(unavailable("read agent stream")(e)), st));
			}
		}
		match st.body.frame().await {
			Some(Ok(frame)) => {
				if let Ok(data) = frame.into_data() {
					st.lines.push(&data);
				}
			}
			Some(Err(e)) => {
				st.done = true;
				return Some((Err(unavailable("read agent stream")(e)), st));
			}
			None => {
				st.done = true;
				return st.lines.finish().map(|line| (parse_event(&line), st));
			}
		}
	}
}

fn parse_event(line: &[u8]) -> Result<proto::Event, ServerError> {
	serde_json::from_slice(line).map_err(unavailable("decode agent event"))
}

#[derive(Debug, PartialEq, Eq)]
struct LineTooLong(usize);

impl Display for LineTooLong {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "line exceeds {} bytes", self.0)
	}
}

fn trim_cr(line: Bytes) -> Bytes {
	if line.ends_with(b"\r") {
		line.slice(..line.len() - 1)
	} else {
		line
	}
}

/// Splits a byte stream into newline-terminated lines across arbitrary chunk
/// boundaries.
struct LineSplitter {
	buf: BytesMut,
	max_line: usize,
}

impl LineSplitter {
	fn new(max_line: usize) -> Self {
		Self {
			buf: BytesMut::new(),
			max_line,
		}
	}

	fn push(&mut self, chunk: &[u8]) {
		self.buf.extend_from_slice(chunk);
	}

	/// Next complete non-empty line without its terminator, or `None` when more
	/// bytes are needed.
	fn next_line(&mut self) -> Result<Option<Bytes>, LineTooLong> {
		loop {
			let Some(end) = self.buf.iter().position(|&b| b == b'\n') else {
				return if self.buf.len() > self.max_line {
					Err(LineTooLong(self.max_line))
				} else {
					Ok(None)
				};
			};
			if end > self.max_line {
				return Err(LineTooLong(self.max_line));
			}
			let line = trim_cr(self.buf.split_to(end + 1).freeze().slice(..end));
			if !line.is_empty() {
				return Ok(Some(line));
			}
		}
	}

	/// The unterminated trailing line at end of stream, if any.
	fn finish(&mut self) -> Option<Bytes> {
		let rest = trim_cr(self.buf.split().freeze());
		(!rest.is_empty()).then_some(rest)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn drain_lines(s: &mut LineSplitter) -> Vec<String> {
		let mut out = Vec::new();
		while let Some(line) = s.next_line().unwrap() {
			out.push(String::from_utf8(line.to_vec()).unwrap());
		}
		out
	}

	#[test]
	fn line_split_across_chunks() {
		let mut s = LineSplitter::new(64);
		s.push(b"{\"a\":");
		assert_eq!(drain_lines(&mut s), Vec::<String>::new());
		s.push(b"1}\n");
		assert_eq!(drain_lines(&mut s), ["{\"a\":1}"]);
		assert_eq!(s.finish(), None);
	}

	#[test]
	fn multiple_lines_in_one_chunk() {
		let mut s = LineSplitter::new(64);
		s.push(b"one\ntwo\r\n\nthree\n");
		assert_eq!(drain_lines(&mut s), ["one", "two", "three"]);
	}

	#[test]
	fn trailing_partial_line_is_returned_by_finish() {
		let mut s = LineSplitter::new(64);
		s.push(b"one\ntwo");
		assert_eq!(drain_lines(&mut s), ["one"]);
		assert_eq!(s.finish().as_deref(), Some(&b"two"[..]));
		assert_eq!(s.finish(), None);
	}

	#[test]
	fn over_cap_line_is_an_error() {
		let mut s = LineSplitter::new(4);
		s.push(b"abcdef");
		assert_eq!(s.next_line(), Err(LineTooLong(4)));

		let mut s = LineSplitter::new(4);
		s.push(b"abcdef\n");
		assert_eq!(s.next_line(), Err(LineTooLong(4)));
	}

	#[test]
	fn line_at_cap_is_accepted() {
		let mut s = LineSplitter::new(4);
		s.push(b"abcd\n");
		assert_eq!(s.next_line().unwrap().as_deref(), Some(&b"abcd"[..]));
	}
}
