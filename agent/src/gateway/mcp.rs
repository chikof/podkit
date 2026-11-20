//! MCP servers behind the gateway.
//!
//! `/mcp/<name>` forwards streamable HTTP
//! (<https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#streamable-http>)
//! to the MCP server a link key opens. The key is the link's own: it opens one
//! server, so a key presented for another name is a 403. The client's
//! credentials are stripped; everything else (method, body, `Mcp-Session-Id`,
//! `Last-Event-ID`, ...) goes through, and the answer, JSON or an SSE stream, is
//! passed back chunk by chunk. Each forwarded request is one count against the
//! link's key.
//!
//! [`tools`] is the same path walked from the other side: a throwaway
//! container makes `initialize` and `tools/list` calls through the gateway, so the
//! UI can show what a server offers (the host cannot reach containers on
//! rootless Podman).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use proto::{Event, GATEWAY_HOST, GATEWAY_PORT, GatewayMcpToolsRequest, Stream};
use tokio::sync::mpsc;

use super::meter::Tokens;
use super::{Failure, Gateway, MAX_REQUEST_BYTES, NOT_FORWARDED, api_key, hash_key};
use crate::ingress::NETWORK;
use crate::model_download::IMAGE;
use crate::podman::{ContainerSpec, Podman};

/// `ANY /mcp/<name>[/rest]`.
pub async fn forward(State(gateway): State<Arc<Gateway>>, request: Request) -> Response {
	match forward_inner(&gateway, request).await {
		Ok(response) => response,
		Err(failure) => axum::response::IntoResponse::into_response(failure),
	}
}

/// Splits `/mcp/<name>/rest` into the name and `/rest` (or nothing).
fn split(path: &str) -> Option<(&str, &str)> {
	let tail = path.strip_prefix("/mcp/")?;
	let (name, rest) = tail.find('/').map_or((tail, ""), |i| tail.split_at(i));
	(!name.is_empty()).then_some((name, rest))
}

async fn forward_inner(gateway: &Gateway, request: Request) -> Result<Response, Failure> {
	let (parts, body) = request.into_parts();
	let table = gateway.table();
	let key = api_key(&parts.headers).ok_or_else(Failure::unauthorized)?;
	let hash = hash_key(key);
	let link = table.mcp.get(&hash).ok_or_else(Failure::unauthorized)?;
	let (name, rest) = split(parts.uri.path())
		.ok_or_else(|| Failure::new(StatusCode::NOT_FOUND, "not_found_error", "no such route"))?;
	if name != link.name {
		return Err(if table.mcp_names.contains(name) {
			Failure::new(
				StatusCode::FORBIDDEN,
				"permission_error",
				format!("this key may not use the MCP server {name:?}"),
			)
		} else {
			Failure::new(
				StatusCode::NOT_FOUND,
				"not_found_error",
				format!("unknown MCP server {name:?}"),
			)
		});
	}
	let bytes = axum::body::to_bytes(body, MAX_REQUEST_BYTES)
		.await
		.map_err(|_| {
			Failure::new(
				StatusCode::PAYLOAD_TOO_LARGE,
				"invalid_request_error",
				"request body is too large",
			)
		})?;

	let mut url = format!("http://{}:{}{}{rest}", link.host, link.port, link.path);
	if let Some(query) = parts.uri.query() {
		url.push('?');
		url.push_str(query);
	}
	let mut headers = HeaderMap::new();
	for (header, value) in &parts.headers {
		if !NOT_FORWARDED.contains(&header.as_str()) {
			headers.append(header.clone(), value.clone());
		}
	}
	let upstream = gateway
		.http
		.request(parts.method, url)
		.headers(headers)
		.body(bytes)
		.send()
		.await
		.map_err(|e| {
			tracing::warn!(mcp = %link.name, error = %e, "gateway: MCP server unreachable");
			Failure::new(
				StatusCode::BAD_GATEWAY,
				"api_error",
				format!("the MCP server {:?} did not answer", link.name),
			)
		})?;
	gateway.count(&hash, link.server_id, true, Tokens::default());

	let mut response = Response::builder().status(upstream.status());
	for (header, value) in upstream.headers() {
		if !matches!(
			header.as_str(),
			"connection" | "keep-alive" | "transfer-encoding"
		) {
			response = response.header(header, value);
		}
	}
	response
		.body(Body::from_stream(upstream.bytes_stream()))
		.map_err(|_| {
			Failure::new(
				StatusCode::BAD_GATEWAY,
				"api_error",
				"the MCP server sent a response that cannot be forwarded",
			)
		})
}

const MEMORY_BYTES: i64 = 128 << 20;
const MAX_TIMEOUT_S: u32 = 120;
/// Image pull and container start, beyond the request's own timeout.
const SLACK: Duration = Duration::from_mins(2);
const KEY_ENV: &str = "PODKIT_GATEWAY_KEY";
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Walks `initialize`, `notifications/initialized` and `tools/list` (all pages) and
/// prints one JSON line. Retries a 401 or a refused connection for a while: the
/// first key of a server makes the agent start the gateway, and the sidecar reads
/// its table a moment after it is written. Answers are JSON or SSE.
const SCRIPT: &str = r#"
import json, os, sys, time, urllib.error, urllib.request
url, timeout = sys.argv[1], float(sys.argv[2])
base = {"content-type": "application/json", "accept": "application/json, text/event-stream",
        "authorization": "Bearer " + os.environ["PODKIT_GATEWAY_KEY"]}
def emit(value):
    print(json.dumps(value), flush=True)
def fail(text):
    emit({"error": text})
    sys.exit(0)
def post(message, headers, retry=False):
    request = urllib.request.Request(url, data=json.dumps(message).encode(), method="POST", headers=headers)
    deadline = time.time() + 25
    while True:
        try:
            return urllib.request.urlopen(request, timeout=timeout)
        except urllib.error.HTTPError as e:
            if retry and e.code == 401 and time.time() < deadline:
                time.sleep(0.5)
                continue
            fail("HTTP %d: %s" % (e.code, e.read(600).decode("utf-8", "replace")))
        except Exception as e:
            if retry and time.time() < deadline:
                time.sleep(0.5)
                continue
            fail(str(e))
def answer(response, want):
    with response:
        kind = response.headers.get("content-type", "")
        if kind.startswith("text/event-stream"):
            for raw in response:
                line = raw.decode("utf-8", "replace").strip()
                if line.startswith("data:"):
                    try:
                        value = json.loads(line[5:].strip())
                    except ValueError:
                        continue
                    if isinstance(value, dict) and value.get("id") == want:
                        return value
            fail("the stream ended without an answer")
        try:
            return json.loads(response.read(8 << 20).decode("utf-8", "replace"))
        except ValueError:
            fail("the answer is not JSON")
def result(value):
    if not isinstance(value, dict):
        fail("the answer is not a JSON-RPC response")
    if "error" in value:
        fail("MCP error: " + str(value["error"].get("message", value["error"]))[:300])
    return value.get("result") or {}
first = post({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-06-18", "capabilities": {},
    "clientInfo": {"name": "podkit", "version": "1"}}}, base, retry=True)
session = first.headers.get("mcp-session-id")
init = result(answer(first, 1))
headers = dict(base)
headers["mcp-protocol-version"] = init.get("protocolVersion") or "2025-06-18"
if session:
    headers["mcp-session-id"] = session
post({"jsonrpc": "2.0", "method": "notifications/initialized"}, headers).close()
tools, cursor = [], None
for page in range(2, 12):
    params = {"cursor": cursor} if cursor else {}
    listed = result(answer(post({"jsonrpc": "2.0", "id": page, "method": "tools/list", "params": params}, headers), page))
    tools.extend(listed.get("tools") or [])
    cursor = listed.get("nextCursor")
    if not cursor:
        break
emit({"tools": tools, "server": init.get("serverInfo") or {}})
"#;

/// Rejects requests that could do anything but list tools.
///
/// # Errors
/// A message naming the bad field.
pub fn validate(req: &GatewayMcpToolsRequest) -> Result<(), String> {
	if req.key.is_empty() || req.key.len() > 200 || req.key.chars().any(char::is_control) {
		return Err("invalid gateway key".to_owned());
	}
	if req.name.is_empty()
		|| req.name.len() > 63
		|| !req
			.name
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
	{
		return Err("invalid MCP server name".to_owned());
	}
	if req.timeout_s == 0 || req.timeout_s > MAX_TIMEOUT_S {
		return Err(format!("timeout must be 1 to {MAX_TIMEOUT_S} seconds"));
	}
	Ok(())
}

fn container_spec(req: &GatewayMcpToolsRequest, name: &str) -> ContainerSpec {
	let url = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/mcp/{}", req.name);
	ContainerSpec {
		name: name.to_owned(),
		image: IMAGE.to_owned(),
		command: Some(vec![
			"python3".to_owned(),
			"-u".to_owned(),
			"-c".to_owned(),
			SCRIPT.to_owned(),
			url,
			req.timeout_s.to_string(),
		]),
		env: vec![(KEY_ENV.to_owned(), req.key.clone())],
		network: NETWORK.to_owned(),
		labels: vec![("podkit.gateway-mcp".to_owned(), "1".to_owned())],
		memory_bytes: Some(MEMORY_BYTES),
		..Default::default()
	}
}

/// Removes the container when the stream ends or the client goes away.
struct Cleanup {
	podman: Podman,
	name: String,
}

impl Drop for Cleanup {
	fn drop(&mut self) {
		let (podman, name) = (self.podman.clone(), std::mem::take(&mut self.name));
		tokio::spawn(async move {
			if let Err(e) = podman.remove(&name).await {
				tracing::warn!(container = %name, error = %e, "removing MCP tools container failed");
			}
		});
	}
}

/// Runs the walk, sending the line the container printed on `tx` and one
/// terminal event.
pub async fn tools(podman: Podman, req: GatewayMcpToolsRequest, tx: mpsc::Sender<Event>) {
	let terminal = match execute(&podman, &req, &tx).await {
		Ok(()) => Event::Done,
		Err(message) => Event::Failed {
			message: message.replace(&req.key, "***"),
		},
	};
	let _ = tx.send(terminal).await;
}

async fn execute(
	podman: &Podman,
	req: &GatewayMcpToolsRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	validate(req)?;
	if podman
		.inspect_image(IMAGE)
		.await
		.map_err(|e| format!("image check failed: {e}"))?
		.is_none()
	{
		let mut pull = std::pin::pin!(podman.pull_image(IMAGE));
		while let Some(item) = pull.next().await {
			item.map_err(|e| format!("image pull failed: {e}"))?;
		}
	}
	let name = format!(
		"podkit-mcp-{}-{}",
		std::process::id(),
		COUNTER.fetch_add(1, Ordering::Relaxed)
	);
	let _cleanup = Cleanup {
		podman: podman.clone(),
		name: name.clone(),
	};
	// Up to 11 requests of `timeout_s` each is the script's worst case.
	tokio::time::timeout(
		Duration::from_secs(u64::from(req.timeout_s) * 3) + SLACK,
		run_container(podman, req, &name, tx),
	)
	.await
	.unwrap_or_else(|_| Err("the MCP container did not finish".to_owned()))
}

async fn run_container(
	podman: &Podman,
	req: &GatewayMcpToolsRequest,
	name: &str,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	podman
		.create_container(container_spec(req, name))
		.await
		.map_err(|e| format!("MCP create failed: {e}"))?;
	podman
		.start(name)
		.await
		.map_err(|e| format!("MCP start failed: {e}"))?;
	let mut pending = String::new();
	let mut logs = std::pin::pin!(podman.logs(name, None, true));
	while let Some(item) = logs.next().await {
		let chunk = item.map_err(|e| format!("reading MCP output failed: {e}"))?;
		pending.push_str(&String::from_utf8_lossy(chunk.as_ref()));
		while let Some(end) = pending.find('\n') {
			let line: String = pending.drain(..=end).collect();
			let line = line.trim_end().to_owned();
			if !line.is_empty()
				&& tx
					.send(Event::Log {
						stream: Stream::Stdout,
						line,
					})
					.await
					.is_err()
			{
				return Ok(());
			}
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests;
