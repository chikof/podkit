//! `podkit-agent --mcp-bridge -- <command>`: puts a stdio MCP server behind
//! streamable HTTP.
//!
//! The agent binary is mounted into the MCP server's container and replaces its
//! entrypoint, so no third-party bridge has to be in the image. The bridge
//! starts the command once and speaks line-delimited JSON-RPC to it over
//! stdin/stdout (MCP stdio transport,
//! <https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#stdio>).
//! It serves the endpoint of the streamable HTTP transport
//! (<https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#streamable-http>):
//!
//! - `POST <path>` takes one JSON-RPC message (or a batch). A request is answered
//!   with `application/json`; notifications and responses with `202 Accepted`.
//! - `GET` and `DELETE <path>` are `405`, which the spec allows a server that
//!   offers no server-initiated stream and no sessions.
//! - `GET /healthz` is `ok`.
//!
//! One child serves every client, so request ids are rewritten on the way in and
//! back on the way out. The child sees one `initialize` and one
//! `notifications/initialized`; a later client's `initialize` is answered with
//! the first one's result. Messages the child sends on its own (notifications,
//! requests to the client) have nobody to go to and are dropped, except `ping`,
//! which is answered, and other requests, which get "method not found" so the
//! child does not wait for them.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ALLOW, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, info, warn};

/// How long a request may wait for the child's answer.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(2);
/// Largest request body.
const MAX_BODY_BYTES: usize = 8 << 20;

const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

type Pending = std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>;

#[derive(Default)]
struct Handshake {
	/// The `result` of the first successful `initialize`.
	result: Option<Value>,
	/// `notifications/initialized` went to the child.
	initialized_sent: bool,
}

/// The running child and the requests waiting for it.
pub struct Bridge {
	lines: mpsc::Sender<String>,
	pending: Pending,
	next_id: AtomicU64,
	handshake: Mutex<Handshake>,
	timeout: Duration,
}

/// A started bridge: the shared state, and the child's exit.
pub struct Started {
	/// Shared state of the running bridge.
	pub bridge: Arc<Bridge>,
	/// Resolves with the child's exit description when it ends.
	pub exited: oneshot::Receiver<String>,
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
	json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

impl Bridge {
	/// Starts `command` and the tasks that talk to it.
	///
	/// # Errors
	/// The command is empty or could not be started.
	pub fn spawn(command: &[String], timeout: Duration) -> Result<Started, String> {
		let (program, args) = command
			.split_first()
			.ok_or("no command to run behind the bridge")?;
		let mut child = Command::new(program)
			.args(args)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			// The server's own log goes to the container log, where podkit shows it.
			.stderr(Stdio::inherit())
			.kill_on_drop(true)
			.spawn()
			.map_err(|e| format!("cannot start {program}: {e}"))?;
		let mut stdin = child.stdin.take().ok_or("no stdin")?;
		let stdout = child.stdout.take().ok_or("no stdout")?;

		let (lines, mut queue) = mpsc::channel::<String>(256);
		let bridge = Arc::new(Self {
			lines,
			pending: std::sync::Mutex::new(HashMap::new()),
			next_id: AtomicU64::new(1),
			handshake: Mutex::new(Handshake::default()),
			timeout,
		});

		tokio::spawn(async move {
			while let Some(line) = queue.recv().await {
				if stdin.write_all(line.as_bytes()).await.is_err()
					|| stdin.write_all(b"\n").await.is_err()
					|| stdin.flush().await.is_err()
				{
					break;
				}
			}
		});

		let reader = bridge.clone();
		tokio::spawn(async move {
			let mut lines = BufReader::new(stdout).lines();
			while let Ok(Some(line)) = lines.next_line().await {
				if !line.trim().is_empty() {
					reader.from_child(&line).await;
				}
			}
		});

		let (done, exited) = oneshot::channel();
		tokio::spawn(async move {
			let status = child.wait().await;
			let _ = done.send(match status {
				Ok(status) => format!("the MCP server exited ({status})"),
				Err(e) => format!("waiting for the MCP server failed: {e}"),
			});
		});
		Ok(Started { bridge, exited })
	}

	/// Handles one line the child printed.
	async fn from_child(&self, line: &str) {
		let Ok(message) = serde_json::from_str::<Value>(line) else {
			debug!(line, "the MCP server printed something that is not JSON");
			return;
		};
		let id = message.get("id").filter(|id| !id.is_null());
		let has_method = message.get("method").is_some();
		match (id, has_method) {
			// A response to one of ours.
			(Some(id), false) => {
				let waiting = id.as_u64().and_then(|id| {
					self.pending
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.remove(&id)
				});
				match waiting {
					Some(waiting) => {
						let _ = waiting.send(message);
					}
					None => debug!(%id, "answer to a request nobody waits for"),
				}
			}
			// A request of the child's own: there is no client to ask.
			(Some(id), true) => {
				let reply = if message.get("method").and_then(Value::as_str) == Some("ping") {
					json!({ "jsonrpc": "2.0", "id": id, "result": {} })
				} else {
					error_response(
						id,
						METHOD_NOT_FOUND,
						"podkit's bridge has no client to answer this request",
					)
				};
				let _ = self.lines.send(reply.to_string()).await;
			}
			(None, _) => debug!("dropped a message the MCP server sent unasked"),
		}
	}

	/// Sends `request` to the child under a fresh id and waits for the answer,
	/// which carries the client's `id` again.
	async fn call(&self, request: &Value) -> Value {
		let client_id = request.get("id").cloned().unwrap_or(Value::Null);
		let id = self.next_id.fetch_add(1, Ordering::Relaxed);
		let mut forwarded = request.clone();
		forwarded["id"] = json!(id);
		let (tx, rx) = oneshot::channel();
		self.pending
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.insert(id, tx);
		if self.lines.send(forwarded.to_string()).await.is_err() {
			self.forget(id);
			return error_response(&client_id, INTERNAL_ERROR, "the MCP server is not running");
		}
		match tokio::time::timeout(self.timeout, rx).await {
			Ok(Ok(mut answer)) => {
				answer["id"] = client_id;
				answer
			}
			Ok(Err(_)) => error_response(&client_id, INTERNAL_ERROR, "the MCP server went away"),
			Err(_) => {
				self.forget(id);
				error_response(
					&client_id,
					INTERNAL_ERROR,
					"the MCP server did not answer in time",
				)
			}
		}
	}

	fn forget(&self, id: u64) {
		self.pending
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.remove(&id);
	}

	async fn request(&self, request: &Value) -> Value {
		if request.get("method").and_then(Value::as_str) != Some("initialize") {
			return self.call(request).await;
		}
		// One handshake for every client: hold the lock across the first one.
		let mut handshake = self.handshake.lock().await;
		if let Some(result) = &handshake.result {
			return json!({
				"jsonrpc": "2.0",
				"id": request.get("id").cloned().unwrap_or(Value::Null),
				"result": result,
			});
		}
		let answer = self.call(request).await;
		if let Some(result) = answer.get("result") {
			handshake.result = Some(result.clone());
		}
		answer
	}

	async fn notification(&self, message: &Value) {
		if message.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
			let mut handshake = self.handshake.lock().await;
			if handshake.initialized_sent {
				return;
			}
			handshake.initialized_sent = true;
		}
		let _ = self.lines.send(message.to_string()).await;
	}

	/// Handles one message of a POST: `Some(answer)` for a request.
	async fn message(&self, message: &Value) -> Option<Value> {
		let has_id = message.get("id").is_some_and(|id| !id.is_null());
		let method = message.get("method").is_some();
		match (method, has_id) {
			(true, true) => Some(self.request(message).await),
			(true, false) => {
				self.notification(message).await;
				None
			}
			// A client's answer to a server request: the bridge never makes any.
			(false, _) => None,
		}
	}
}

/// The bridge's routes: the endpoint at `path`, and `/healthz`.
pub fn router(bridge: Arc<Bridge>, path: &str) -> Router {
	Router::new()
		.route(path, get(not_allowed).delete(not_allowed).post(post))
		.route("/healthz", get(|| async { "ok" }))
		.with_state(bridge)
}

async fn not_allowed() -> Response {
	let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
	response
		.headers_mut()
		.insert(ALLOW, HeaderValue::from_static("POST"));
	response
}

fn json_response(status: StatusCode, body: &Value) -> Response {
	(
		status,
		[(CONTENT_TYPE, "application/json")],
		body.to_string(),
	)
		.into_response()
}

async fn post(State(bridge): State<Arc<Bridge>>, body: Bytes) -> Response {
	if body.len() > MAX_BODY_BYTES {
		return StatusCode::PAYLOAD_TOO_LARGE.into_response();
	}
	let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
		return json_response(
			StatusCode::BAD_REQUEST,
			&error_response(&Value::Null, PARSE_ERROR, "the body is not JSON"),
		);
	};
	match parsed {
		Value::Array(messages) => {
			let mut answers = Vec::new();
			for message in &messages {
				answers.extend(bridge.message(message).await);
			}
			if answers.is_empty() {
				StatusCode::ACCEPTED.into_response()
			} else {
				json_response(StatusCode::OK, &Value::Array(answers))
			}
		}
		message @ Value::Object(_) => match bridge.message(&message).await {
			Some(answer) => json_response(StatusCode::OK, &answer),
			None => StatusCode::ACCEPTED.into_response(),
		},
		_ => json_response(
			StatusCode::BAD_REQUEST,
			&error_response(&Value::Null, PARSE_ERROR, "expected a JSON-RPC message"),
		),
	}
}

/// Runs the bridge until the child exits (which is an error: the container
/// should restart) or SIGTERM.
///
/// # Errors
/// The command or the listener could not be started, or the child exited.
pub async fn serve(listen: &str, path: &str, command: &[String]) -> Result<(), String> {
	if !path.starts_with('/') {
		return Err(format!("the path {path:?} must start with /"));
	}
	let started = Bridge::spawn(command, REQUEST_TIMEOUT)?;
	let listener = tokio::net::TcpListener::bind(listen)
		.await
		.map_err(|e| format!("cannot listen on {listen}: {e}"))?;
	info!(%listen, %path, "MCP bridge listening");
	let server = axum::serve(listener, router(started.bridge, path));
	tokio::select! {
		result = server => result.map_err(|e| e.to_string()),
		exited = started.exited => {
			let message = exited.unwrap_or_else(|_| "the MCP server ended".to_owned());
			warn!("{message}");
			Err(message)
		}
		() = terminate() => Ok(()),
	}
}

async fn terminate() {
	use tokio::signal::unix::{SignalKind, signal};
	let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
	tokio::select! {
		_ = term.recv() => {}
		_ = tokio::signal::ctrl_c() => {}
	}
}

#[cfg(test)]
mod tests;
