//! podkit as a Model Context Protocol server, so an AI agent can operate podkit with a
//! scoped API token.
//!
//! The protocol is small (JSON-RPC 2.0 over one `POST /mcp`), so it is implemented here
//! instead of pulling in an SDK and its dependency tree; the transport follows the
//! "Streamable HTTP" section of the spec, answering every request with a plain JSON body
//! (a server may choose JSON over an event stream) and holding no session state:
//! <https://modelcontextprotocol.io/specification/2025-06-18/basic/transports>.
//!
//! Nothing here decides who may do what. Every tool turns into a request to podkit's own
//! JSON API, carrying the caller's own token, so the token gate (scope, team, project
//! limit), RBAC and the audit log apply exactly as they do for a script. The audit log
//! records those calls with actor kind `mcp` ([`crate::audit::as_mcp`]).
//!
//! `podkit mcp` (the `cli` crate) is the stdio transport: it relays each line to this
//! endpoint, so the tools live in one place.

mod service;
#[cfg(test)]
mod tests;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use crate::audit;
use crate::auth::tokens::{self, PREFIX};
use crate::error::ServerError;
use crate::state::AppState;

/// The JSON API router, with its state, that tool calls are sent through.
#[derive(Clone)]
pub struct Api(pub Router);

/// Protocol revisions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// `POST /mcp`. Mounted outside the JSON API so it can answer in JSON-RPC, but it only
/// takes API tokens (a session is a person, not an agent).
pub fn router(api: Router) -> Router<AppState> {
	Router::new()
		.route("/mcp", post(handle).get(not_allowed).delete(not_allowed))
		.layer(Extension(Api(api)))
}

/// A `GET` would open a server-to-client event stream and a `DELETE` would end a session;
/// this server has neither.
async fn not_allowed() -> Response {
	(StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

/// Browsers reach `localhost` servers from any page; refuse a request whose `Origin` is
/// not this server's own host (the spec asks for this against DNS rebinding).
fn origin_ok(headers: &HeaderMap) -> bool {
	let Some(origin) = headers.get(header::ORIGIN) else {
		return true;
	};
	let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
	let origin = origin.to_str().ok();
	match (origin, host) {
		(Some(o), Some(h)) => o
			.split_once("://")
			.is_some_and(|(_, authority)| authority.eq_ignore_ascii_case(h)),
		_ => false,
	}
}

async fn handle(
	State(state): State<AppState>,
	Extension(Api(api)): Extension<Api>,
	headers: HeaderMap,
	body: Bytes,
) -> Response {
	if !origin_ok(&headers) {
		return ServerError::Forbidden.into_response();
	}
	let Some(authorization) = headers
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
	else {
		return ServerError::InvalidToken.into_response();
	};
	let secret = authorization
		.split_once(' ')
		.filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
		.map(|(_, t)| t.trim())
		.filter(|t| t.starts_with(PREFIX));
	let Some(secret) = secret else {
		return ServerError::InvalidToken.into_response();
	};
	let grant = match tokens::service::resolve(&state.db, secret).await {
		Ok(grant) => grant,
		Err(e) => return e.into_response(),
	};
	let session = service::Session::new(api, format!("Bearer {secret}"), grant);

	let Ok(message) = serde_json::from_slice::<Value>(&body) else {
		return json_response(&failure(&Value::Null, -32700, "parse error"));
	};
	let reply = audit::as_mcp(async {
		match message {
			// Batches were allowed up to 2025-03-26; answering them costs nothing.
			Value::Array(items) if !items.is_empty() => {
				let mut out = Vec::new();
				for item in items {
					out.extend(dispatch(&session, item).await);
				}
				(!out.is_empty()).then_some(Value::Array(out))
			}
			other => dispatch(&session, other).await,
		}
	})
	.await;
	// Notifications and responses get no body, only acknowledgement.
	reply.map_or_else(
		|| StatusCode::ACCEPTED.into_response(),
		|v| json_response(&v),
	)
}

fn json_response(value: &Value) -> Response {
	(
		[(header::CONTENT_TYPE, "application/json")],
		value.to_string(),
	)
		.into_response()
}

fn success(id: &Value, result: &Value) -> Value {
	json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn failure(id: &Value, code: i64, message: &str) -> Value {
	json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Answers one JSON-RPC message; `None` for a notification, which has no reply.
async fn dispatch(session: &service::Session, message: Value) -> Option<Value> {
	let Some(object) = message.as_object() else {
		return Some(failure(&Value::Null, -32600, "invalid request"));
	};
	let id = object.get("id").cloned()?;
	let Some(method) = object.get("method").and_then(Value::as_str) else {
		// A response to something we never sent.
		return None;
	};
	let params = object.get("params").cloned().unwrap_or(Value::Null);
	Some(match run(session, method, &params).await {
		Ok(result) => success(&id, &result),
		Err((code, message)) => failure(&id, code, &message),
	})
}

type RpcResult = Result<Value, (i64, String)>;

fn invalid(message: impl Into<String>) -> (i64, String) {
	(-32602, message.into())
}

async fn run(session: &service::Session, method: &str, params: &Value) -> RpcResult {
	match method {
		"initialize" => Ok(initialize(params)),
		"ping" => Ok(json!({})),
		"tools/list" => Ok(json!({ "tools": service::tools() })),
		"tools/call" => {
			let name = params
				.get("name")
				.and_then(Value::as_str)
				.ok_or_else(|| invalid("missing tool name"))?;
			let args = params
				.get("arguments")
				.cloned()
				.unwrap_or_else(|| json!({}));
			if !service::tools()
				.iter()
				.any(|t| t["name"].as_str() == Some(name))
			{
				return Err(invalid(format!("unknown tool {name}")));
			}
			// A failed tool is a result the model can read and react to, not a
			// protocol error.
			Ok(match session.call(name, &args).await {
				Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
				Err(message) => json!({
					"isError": true,
					"content": [{ "type": "text", "text": message }],
				}),
			})
		}
		"resources/list" => session.resources().await.map_err(|m| (-32603, m)),
		"resources/templates/list" => Ok(service::resource_templates()),
		"resources/read" => {
			let uri = params
				.get("uri")
				.and_then(Value::as_str)
				.ok_or_else(|| invalid("missing uri"))?;
			session.read(uri).await.map_err(invalid)
		}
		"prompts/list" => Ok(service::prompts()),
		"prompts/get" => service::prompt(params).map_err(invalid),
		other => Err((-32601, format!("method not found: {other}"))),
	}
}

/// Picks the newest revision both sides know: the client's own if we speak it, else ours.
fn initialize(params: &Value) -> Value {
	let asked = params.get("protocolVersion").and_then(Value::as_str);
	let version = VERSIONS
		.iter()
		.find(|v| Some(**v) == asked)
		.unwrap_or(&VERSIONS[0]);
	json!({
		"protocolVersion": version,
		"capabilities": {
			"tools": { "listChanged": false },
			"resources": { "listChanged": false, "subscribe": false },
			"prompts": { "listChanged": false },
		},
		"serverInfo": { "name": "podkit", "version": env!("CARGO_PKG_VERSION") },
		"instructions": "podkit manages containers, databases and models on your servers. \
			Your token's scopes limit what the tools may do; a refused call says which scope \
			is missing. Secrets are never returned. Start with list_projects.",
	})
}
