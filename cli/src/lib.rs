//! The `podkit` command line. Most of it ([`commands`]) works over the JSON API with an
//! API token; this file is the stdio side of podkit's MCP server.
//!
//! An MCP client (Claude Desktop, Claude Code, Hermes) starts `podkit mcp` and talks to
//! it over stdin and stdout, one JSON-RPC message per line, as the spec's stdio transport
//! says: <https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#stdio>.
//! This process holds no tools of its own. It relays each line to `POST {PODKIT_URL}/mcp`
//! with the API token, and writes the reply back as one line. Scope, RBAC and audit are
//! all decided by the server.

use std::io;

pub mod api;
pub mod args;
pub mod commands;
pub mod config;
pub mod dotenv;
pub mod import;
pub mod sse;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// JSON-RPC code for "the server could not be reached or refused us".
const TRANSPORT_ERROR: i64 = -32000;

/// Where to send the lines and as whom.
pub struct Target {
	/// Full URL of the server's MCP endpoint.
	pub url: String,
	/// The API token (`pk_…`).
	pub token: String,
}

impl Target {
	/// A target from podkit's base URL (`http://host:8080`) and a token.
	#[must_use]
	pub fn new(base: &str, token: &str) -> Self {
		Self {
			url: format!("{}/mcp", base.trim_end_matches('/')),
			token: token.to_owned(),
		}
	}
}

/// The HTTP client. An `https` podkit needs a TLS crypto provider installed first.
#[must_use]
pub fn client() -> reqwest::Client {
	// Already installed by an earlier call is fine.
	let _ = rustls::crypto::ring::default_provider().install_default();
	reqwest::Client::new()
}

/// Reads messages from `input` until it closes, relaying each to the server and writing
/// the replies to `output`. Messages are handled one at a time, in order.
///
/// # Errors
/// Fails only when reading `input` or writing `output` does.
pub async fn relay<R, W>(
	input: R,
	mut output: W,
	client: &reqwest::Client,
	target: &Target,
) -> io::Result<()>
where
	R: AsyncBufRead + Unpin,
	W: AsyncWrite + Unpin,
{
	let mut lines = input.lines();
	while let Some(line) = lines.next_line().await? {
		if line.trim().is_empty() {
			continue;
		}
		for reply in exchange(client, target, &line).await {
			output.write_all(reply.as_bytes()).await?;
			output.write_all(b"\n").await?;
		}
		output.flush().await?;
	}
	Ok(())
}

/// One message to the server and what to write back for it: nothing for a notification,
/// the server's reply otherwise, or a JSON-RPC error when the server was not reachable.
async fn exchange(client: &reqwest::Client, target: &Target, line: &str) -> Vec<String> {
	let id = serde_json::from_str::<Value>(line)
		.ok()
		.and_then(|m| m.get("id").cloned());
	let failed = |what: String| {
		if let Some(id) = &id {
			vec![
				json!({
					"jsonrpc": "2.0",
					"id": id,
					"error": { "code": TRANSPORT_ERROR, "message": what },
				})
				.to_string(),
			]
		} else {
			eprintln!("podkit mcp: {what}");
			Vec::new()
		}
	};

	let response = client
		.post(&target.url)
		.bearer_auth(&target.token)
		.header("content-type", "application/json")
		.header("accept", "application/json, text/event-stream")
		.body(line.to_owned())
		.send()
		.await;
	let response = match response {
		Ok(r) => r,
		Err(e) => return failed(format!("could not reach podkit at {}: {e}", target.url)),
	};
	let status = response.status();
	let event_stream = response
		.headers()
		.get("content-type")
		.and_then(|v| v.to_str().ok())
		.is_some_and(|v| v.starts_with("text/event-stream"));
	let body = match response.text().await {
		Ok(b) => b,
		Err(e) => return failed(format!("podkit's answer was cut off: {e}")),
	};
	if !status.is_success() {
		let detail = serde_json::from_str::<Value>(&body)
			.ok()
			.and_then(|v| v["error"].as_str().map(str::to_owned))
			.unwrap_or_default();
		return failed(format!("podkit answered {}: {detail}", status.as_u16()));
	}
	// 202 Accepted: a notification, nothing to say back.
	if body.trim().is_empty() {
		return Vec::new();
	}
	let messages: Vec<&str> = if event_stream {
		body.lines()
			.filter_map(|l| l.strip_prefix("data:"))
			.map(str::trim)
			.filter(|d| !d.is_empty())
			.collect()
	} else {
		vec![body.trim()]
	};
	// One message per line: re-serialising guarantees no stray newline inside one.
	messages
		.into_iter()
		.filter_map(|m| serde_json::from_str::<Value>(m).ok())
		.map(|v| v.to_string())
		.collect()
}
