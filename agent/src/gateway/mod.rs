//! The LLM gateway: a thin pass-through proxy in front of the model engines.
//!
//! Clients see one URL and their own key. The gateway looks the key up, picks
//! the engine by the request's `model` field, forwards the request with the
//! credentials stripped, streams the answer back without buffering, and counts
//! the tokens it saw per key, model and hour. There is no API translation:
//! the `OpenAI` and Anthropic routes are forwarded as they are.
//!
//! It runs as a sidecar container of the agent binary (`podkit-agent
//! --gateway`), because on rootless Podman only containers on the `podkit`
//! network can reach the engines. See `docs/gateway.md`.

pub mod chat;
pub mod mcp;
mod meter;
pub mod sidecar;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use proto::{GatewayConfig, GatewayMcp, GatewayModel, GatewayUsage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use meter::{Meter, Tokens};

/// Largest request body the gateway reads (it has to, to find the model).
const MAX_REQUEST_BYTES: usize = 64 << 20;
const X_API_KEY: HeaderName = HeaderName::from_static("x-api-key");
/// Request headers that are not forwarded. `accept-encoding` goes too, so the
/// engine answers in plain text and the usage can be read.
const NOT_FORWARDED: [&str; 11] = [
	"authorization",
	"x-api-key",
	"host",
	"content-length",
	"connection",
	"keep-alive",
	"transfer-encoding",
	"te",
	"upgrade",
	"proxy-authorization",
	"accept-encoding",
];

#[derive(Default)]
struct Table {
	keys: HashMap<String, Vec<GatewayModel>>,
	other: HashSet<String>,
	/// Link keys, by hash: the one MCP server each opens.
	mcp: HashMap<String, GatewayMcp>,
	/// Names of every MCP server the server knows, to tell 403 from 404.
	mcp_names: HashSet<String>,
}

#[derive(Debug, Default)]
struct Counter {
	requests: u64,
	input: u64,
	output: u64,
}

/// Counter key: key hash, model (or MCP server) id, hour, and whether it is an MCP link's.
type UsageKey = (String, i64, i64, bool);

/// Gateway state: the routing table and the usage counters.
pub struct Gateway {
	table: RwLock<Arc<Table>>,
	usage: Mutex<HashMap<UsageKey, Counter>>,
	http: reqwest::Client,
}

/// Hex SHA-256 of an API key, as the server stores and pushes it.
#[must_use]
pub fn hash_key(key: &str) -> String {
	use std::fmt::Write;
	Sha256::digest(key.as_bytes())
		.iter()
		.fold(String::with_capacity(64), |mut out, b| {
			let _ = write!(out, "{b:02x}");
			out
		})
}

fn hour_of(now: SystemTime) -> i64 {
	let secs = now
		.duration_since(UNIX_EPOCH)
		.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
	secs - secs.rem_euclid(3600)
}

impl Gateway {
	/// # Errors
	/// The HTTP client could not be built.
	pub fn new() -> Result<Arc<Self>, reqwest::Error> {
		Ok(Arc::new(Self {
			table: RwLock::new(Arc::new(Table::default())),
			usage: Mutex::new(HashMap::new()),
			http: reqwest::Client::builder()
				.connect_timeout(Duration::from_secs(10))
				// Between two reads, not overall: a long answer is fine, a silent engine is not.
				.read_timeout(Duration::from_mins(10))
				.pool_idle_timeout(Duration::from_secs(30))
				.build()?,
		}))
	}

	/// Replaces the routing table.
	pub fn set_config(&self, config: GatewayConfig) {
		let mcp_names = config
			.other_mcp
			.iter()
			.cloned()
			.chain(config.mcp_keys.iter().map(|k| k.mcp.name.clone()))
			.collect();
		let table = Table {
			keys: config
				.keys
				.into_iter()
				.map(|k| (k.key_hash, k.models))
				.collect(),
			other: config.other_models.into_iter().collect(),
			mcp: config
				.mcp_keys
				.into_iter()
				.map(|k| (k.key_hash, k.mcp))
				.collect(),
			mcp_names,
		};
		*self
			.table
			.write()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(table);
	}

	fn table(&self) -> Arc<Table> {
		self.table
			.read()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.clone()
	}

	fn record(&self, key_hash: &str, model_id: i64, tokens: Tokens) {
		self.count(key_hash, model_id, false, tokens);
	}

	fn count(&self, key_hash: &str, id: i64, mcp: bool, tokens: Tokens) {
		let hour = hour_of(SystemTime::now());
		let mut usage = self
			.usage
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let counter = usage
			.entry((key_hash.to_owned(), id, hour, mcp))
			.or_default();
		counter.requests += 1;
		counter.input += tokens.input;
		counter.output += tokens.output;
	}

	/// Takes the counters collected since the last call.
	pub fn take_usage(&self) -> Vec<GatewayUsage> {
		let taken = std::mem::take(
			&mut *self
				.usage
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner),
		);
		taken
			.into_iter()
			.map(|((key_hash, model_id, hour, mcp), c)| GatewayUsage {
				key_hash,
				model_id,
				hour,
				requests: c.requests,
				input_tokens: c.input,
				output_tokens: c.output,
				mcp,
			})
			.collect()
	}
}

/// The gateway's routes.
pub fn router(gateway: Arc<Gateway>) -> Router {
	Router::new()
		.route("/v1/models", get(list_models))
		.route("/v1/chat/completions", post(forward))
		.route("/v1/completions", post(forward))
		.route("/v1/embeddings", post(forward))
		.route("/v1/messages", post(forward))
		.route("/mcp/{*tail}", any(mcp::forward))
		.route("/healthz", get(|| async { "ok" }))
		.layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
		.with_state(gateway)
}

/// An error in the shape both the `OpenAI` and the Anthropic SDKs read.
struct Failure {
	status: StatusCode,
	kind: &'static str,
	message: String,
}

impl Failure {
	fn new(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Self {
		Self {
			status,
			kind,
			message: message.into(),
		}
	}

	fn unauthorized() -> Self {
		Self::new(
			StatusCode::UNAUTHORIZED,
			"authentication_error",
			"invalid or missing API key",
		)
	}

	fn bad_request(message: &str) -> Self {
		Self::new(StatusCode::BAD_REQUEST, "invalid_request_error", message)
	}
}

impl IntoResponse for Failure {
	fn into_response(self) -> Response {
		(
			self.status,
			Json(json!({
				"type": "error",
				"error": { "type": self.kind, "message": self.message },
			})),
		)
			.into_response()
	}
}

/// The key from `Authorization: Bearer` or `x-api-key`.
fn api_key(headers: &HeaderMap) -> Option<&str> {
	let bearer = headers
		.get(AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| {
			let (scheme, token) = v.split_once(' ')?;
			scheme.eq_ignore_ascii_case("bearer").then_some(token)
		});
	bearer
		.or_else(|| headers.get(&X_API_KEY).and_then(|v| v.to_str().ok()))
		.map(str::trim)
		.filter(|k| !k.is_empty())
}

/// Models the request's key may use, or the 401.
fn models_of<'a>(
	table: &'a Table,
	headers: &HeaderMap,
) -> Result<(String, &'a [GatewayModel]), Failure> {
	let key = api_key(headers).ok_or_else(Failure::unauthorized)?;
	let hash = hash_key(key);
	let models = table.keys.get(&hash).ok_or_else(Failure::unauthorized)?;
	Ok((hash, models))
}

/// `GET /v1/models`: what this key may use, in `OpenAI`'s list format.
async fn list_models(
	State(gateway): State<Arc<Gateway>>,
	headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
	let table = gateway.table();
	let (_, models) = models_of(&table, &headers)?;
	Ok(Json(json!({
		"object": "list",
		"data": models.iter().map(|m| json!({
			"id": m.name, "object": "model", "created": 0, "owned_by": "podkit",
		})).collect::<Vec<_>>(),
	})))
}

/// Whether `path` is an `OpenAI`-style route that takes `stream_options`.
fn takes_stream_options(path: &str) -> bool {
	matches!(path, "/v1/chat/completions" | "/v1/completions")
}

/// Asks the engine for a final usage chunk on a stream, when the client did
/// not decide either way. Returns whether the body changed.
fn with_stream_usage(body: &mut Value) -> bool {
	let Some(object) = body.as_object_mut() else {
		return false;
	};
	let options = object.entry("stream_options").or_insert(Value::Null);
	if options.is_null() {
		*options = json!({ "include_usage": true });
		return true;
	}
	match options.as_object_mut() {
		Some(options) if !options.contains_key("include_usage") => {
			options.insert("include_usage".to_owned(), Value::Bool(true));
			true
		}
		_ => false,
	}
}

/// A request that passed the checks: where it goes and what is sent.
struct Routed<'a> {
	key_hash: String,
	model: &'a GatewayModel,
	body: Bytes,
}

/// Checks the key and the model, and works out the body to send.
async fn route<'a>(table: &'a Table, parts: &Parts, body: Body) -> Result<Routed<'a>, Failure> {
	let (key_hash, allowed) = models_of(table, &parts.headers)?;
	let bytes = axum::body::to_bytes(body, MAX_REQUEST_BYTES)
		.await
		.map_err(|_| {
			Failure::new(
				StatusCode::PAYLOAD_TOO_LARGE,
				"invalid_request_error",
				"request body is too large",
			)
		})?;
	let mut json = serde_json::from_slice::<Value>(&bytes)
		.map_err(|_| Failure::bad_request("request body is not JSON"))?;
	let name = json
		.get("model")
		.and_then(Value::as_str)
		.ok_or_else(|| Failure::bad_request("the request has no \"model\""))?;
	let Some(model) = allowed.iter().find(|m| m.name == name) else {
		if table.other.contains(name) {
			return Err(Failure::new(
				StatusCode::FORBIDDEN,
				"permission_error",
				format!("this key may not use the model {name:?}"),
			));
		}
		let available: Vec<&str> = allowed.iter().map(|m| m.name.as_str()).collect();
		return Err(Failure::new(
			StatusCode::NOT_FOUND,
			"not_found_error",
			format!(
				"unknown model {name:?}; available models: {}",
				available.join(", ")
			),
		));
	};

	let streaming = json.get("stream").and_then(Value::as_bool) == Some(true);
	let body = if streaming
		&& model.stream_usage
		&& takes_stream_options(parts.uri.path())
		&& with_stream_usage(&mut json)
	{
		serde_json::to_vec(&json).map_or(bytes, Into::into)
	} else {
		bytes
	};
	Ok(Routed {
		key_hash,
		model,
		body,
	})
}

/// The five forwarded POST routes.
async fn forward(
	State(gateway): State<Arc<Gateway>>,
	request: Request,
) -> Result<Response, Failure> {
	let (parts, body) = request.into_parts();
	let table = gateway.table();
	let Routed {
		key_hash,
		model,
		body,
	} = route(&table, &parts, body).await?;

	let url = format!("http://{}:{}{}", model.host, model.port, parts.uri.path());
	let mut headers = HeaderMap::new();
	for (name, value) in &parts.headers {
		if !NOT_FORWARDED.contains(&name.as_str()) {
			headers.append(name.clone(), value.clone());
		}
	}
	let upstream = gateway
		.http
		.post(url)
		.headers(headers)
		.body(body)
		.send()
		.await
		.map_err(|e| {
			tracing::warn!(model = %model.name, error = %e, "gateway: engine unreachable");
			Failure::new(
				StatusCode::BAD_GATEWAY,
				"api_error",
				format!("the engine for {:?} did not answer", model.name),
			)
		})?;

	let sse = upstream
		.headers()
		.get(CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.is_some_and(|v| v.starts_with("text/event-stream"));
	let mut response = Response::builder().status(upstream.status());
	for (name, value) in upstream.headers() {
		if !matches!(
			name.as_str(),
			"connection" | "keep-alive" | "transfer-encoding"
		) {
			response = response.header(name, value);
		}
	}
	let mut meter = Meter::new(gateway.clone(), key_hash, model.model_id, sse);
	let stream = upstream.bytes_stream().map(move |chunk| {
		if let Ok(chunk) = &chunk {
			meter.feed(chunk);
		}
		chunk
	});
	response.body(Body::from_stream(stream)).map_err(|_| {
		Failure::new(
			StatusCode::BAD_GATEWAY,
			"api_error",
			"the engine sent a response that cannot be forwarded",
		)
	})
}

#[cfg(test)]
mod tests;
