use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use proto::{GatewayConfig, GatewayKey, GatewayModel};
use serde_json::{Value, json};

use super::{Gateway, hash_key, router};

const KEY: &str = "pk-test-key";

/// What the stub engine received.
#[derive(Default)]
struct Seen {
	bodies: Vec<Value>,
	headers: Vec<HeaderMap>,
}

type Stub = Arc<Mutex<Seen>>;

fn sse(events: Vec<(Duration, String)>) -> Response {
	let stream = futures_util::stream::unfold(events.into_iter(), |mut events| async move {
		let (delay, event) = events.next()?;
		tokio::time::sleep(delay).await;
		Some((Ok::<_, Infallible>(Bytes::from(event)), events))
	});
	Response::builder()
		.header(header::CONTENT_TYPE, "text/event-stream")
		.body(Body::from_stream(stream))
		.unwrap()
}

fn data(value: &Value) -> String {
	format!("data: {value}\n\n")
}

/// An `OpenAI`-style engine. A stream sends its first chunk at once, then waits
/// `SLOW` before the rest.
async fn openai(State(seen): State<Stub>, headers: HeaderMap, body: Bytes) -> Response {
	let body: Value = serde_json::from_slice(&body).unwrap();
	let streaming = body["stream"] == true;
	let include_usage = body["stream_options"]["include_usage"] == true;
	{
		let mut seen = seen.lock().unwrap();
		seen.bodies.push(body);
		seen.headers.push(headers);
	}
	if !streaming {
		return axum::Json(json!({
			"choices": [{"message": {"content": "hello"}}],
			"usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10},
		}))
		.into_response();
	}
	let mut events = vec![
		(
			Duration::ZERO,
			data(&json!({"choices": [{"delta": {"content": "he"}}], "usage": null})),
		),
		(
			SLOW,
			data(&json!({"choices": [{"delta": {"content": "llo"}}], "usage": null})),
		),
	];
	if include_usage {
		events.push((
			Duration::ZERO,
			data(&json!({"choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 3}})),
		));
	}
	events.push((Duration::ZERO, "data: [DONE]\n\n".to_owned()));
	sse(events)
}

const SLOW: Duration = Duration::from_millis(800);

/// An Anthropic-style engine.
async fn anthropic(State(seen): State<Stub>, headers: HeaderMap, body: Bytes) -> Response {
	let body: Value = serde_json::from_slice(&body).unwrap();
	let streaming = body["stream"] == true;
	{
		let mut seen = seen.lock().unwrap();
		seen.bodies.push(body);
		seen.headers.push(headers);
	}
	if !streaming {
		return axum::Json(json!({
			"type": "message",
			"content": [{"type": "text", "text": "hello"}],
			"usage": {"input_tokens": 4, "cache_read_input_tokens": 6, "output_tokens": 5},
		}))
		.into_response();
	}
	let event = |name: &str, value: Value| format!("event: {name}\ndata: {value}\n\n");
	sse(vec![
		(
			Duration::ZERO,
			event(
				"message_start",
				json!({"type": "message_start",
					"message": {"usage": {"input_tokens": 12, "output_tokens": 0}}}),
			),
		),
		(
			SLOW,
			event(
				"content_block_delta",
				json!({"type": "content_block_delta", "delta": {"text": "hi"}}),
			),
		),
		(
			Duration::ZERO,
			event(
				"message_delta",
				json!({"type": "message_delta", "usage": {"output_tokens": 9}}),
			),
		),
		(
			Duration::ZERO,
			event("message_stop", json!({"type": "message_stop"})),
		),
	])
}

async fn serve(app: Router) -> u16 {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	port
}

struct Harness {
	gateway: Arc<Gateway>,
	url: String,
	seen: Stub,
	client: reqwest::Client,
}

fn model(id: i64, name: &str, port: u16, stream_usage: bool) -> GatewayModel {
	GatewayModel {
		model_id: id,
		name: name.to_owned(),
		host: "127.0.0.1".to_owned(),
		port,
		stream_usage,
	}
}

/// One stub engine serving both APIs, behind a gateway whose key may use
/// `qwen` (with `stream_usage`) and `plain` (without), and which knows
/// of a model `secret` the key may not use.
async fn harness() -> Harness {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let seen: Stub = Stub::default();
	let engine = Router::new()
		.route("/v1/chat/completions", post(openai))
		.route("/v1/completions", post(openai))
		.route("/v1/embeddings", post(openai))
		.route("/v1/messages", post(anthropic))
		.with_state(seen.clone());
	let port = serve(engine).await;

	let gateway = Gateway::new().unwrap();
	gateway.set_config(GatewayConfig {
		keys: vec![GatewayKey {
			key_hash: hash_key(KEY),
			models: vec![model(1, "qwen", port, true), model(2, "plain", port, false)],
		}],
		other_models: vec!["secret".to_owned()],
	});
	let url = format!("http://127.0.0.1:{}", serve(router(gateway.clone())).await);
	Harness {
		gateway,
		url,
		seen,
		client: reqwest::Client::new(),
	}
}

impl Harness {
	fn post(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
		self.client
			.post(format!("{}{path}", self.url))
			.bearer_auth(KEY)
			.json(body)
	}

	/// Totals of `(requests, input, output)` once `requests` requests are counted.
	async fn usage(&self, requests: u64) -> (u64, u64, u64) {
		let mut total = (0, 0, 0);
		for _ in 0..100 {
			for row in self.gateway.take_usage() {
				total.0 += row.requests;
				total.1 += row.input_tokens;
				total.2 += row.output_tokens;
			}
			if total.0 >= requests {
				return total;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
		total
	}
}

fn chat(model: &str, stream: bool) -> Value {
	json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "stream": stream})
}

#[tokio::test]
async fn gateway_wrong_or_missing_key_is_401() {
	let h = harness().await;
	let body = chat("qwen", false);
	let no_key = h
		.client
		.post(format!("{}/v1/chat/completions", h.url))
		.json(&body)
		.send()
		.await
		.unwrap();
	assert_eq!(no_key.status(), StatusCode::UNAUTHORIZED);
	let wrong = h
		.client
		.post(format!("{}/v1/chat/completions", h.url))
		.bearer_auth("nope")
		.json(&body)
		.send()
		.await
		.unwrap();
	assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
	let wrong_x = h
		.client
		.post(format!("{}/v1/messages", h.url))
		.header("x-api-key", "nope")
		.json(&body)
		.send()
		.await
		.unwrap();
	assert_eq!(wrong_x.status(), StatusCode::UNAUTHORIZED);
	let models = h
		.client
		.get(format!("{}/v1/models", h.url))
		.send()
		.await
		.unwrap();
	assert_eq!(models.status(), StatusCode::UNAUTHORIZED);
	assert!(h.seen.lock().unwrap().bodies.is_empty());
}

#[tokio::test]
async fn gateway_model_not_allowed_is_403_and_unknown_is_404_with_the_names() {
	let h = harness().await;
	let forbidden = h
		.post("/v1/chat/completions", &chat("secret", false))
		.send()
		.await
		.unwrap();
	assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

	let unknown = h
		.post("/v1/chat/completions", &chat("nonesuch", false))
		.send()
		.await
		.unwrap();
	assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
	let body: Value = unknown.json().await.unwrap();
	let message = body["error"]["message"].as_str().unwrap();
	assert!(
		message.contains("qwen") && message.contains("plain"),
		"{message}"
	);
	assert!(!message.contains("secret"), "{message}");

	let missing = h
		.post("/v1/chat/completions", &json!({"messages": []}))
		.send()
		.await
		.unwrap();
	assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
	assert!(h.seen.lock().unwrap().bodies.is_empty());
}

#[tokio::test]
async fn gateway_lists_the_models_of_the_key() {
	let h = harness().await;
	let list: Value = h
		.client
		.get(format!("{}/v1/models", h.url))
		.header("x-api-key", KEY)
		.send()
		.await
		.unwrap()
		.json()
		.await
		.unwrap();
	let ids: Vec<&str> = list["data"]
		.as_array()
		.unwrap()
		.iter()
		.map(|m| m["id"].as_str().unwrap())
		.collect();
	assert_eq!(ids, ["qwen", "plain"]);
}

#[tokio::test]
async fn gateway_forwards_openai_requests_without_the_credentials_and_counts_usage() {
	let h = harness().await;
	let reply: Value = h
		.post("/v1/chat/completions", &chat("qwen", false))
		.header("x-api-key", "also-stripped")
		.header("anthropic-version", "2023-06-01")
		.send()
		.await
		.unwrap()
		.json()
		.await
		.unwrap();
	assert_eq!(reply["choices"][0]["message"]["content"], "hello");

	{
		let seen = h.seen.lock().unwrap();
		let headers = &seen.headers[0];
		assert!(headers.get(header::AUTHORIZATION).is_none());
		assert!(headers.get("x-api-key").is_none());
		assert_eq!(headers["anthropic-version"], "2023-06-01");
		assert_eq!(seen.bodies[0]["model"], "qwen");
	}
	assert_eq!(h.usage(1).await, (1, 7, 3));
}

#[tokio::test]
async fn gateway_accepts_the_x_api_key_header_on_every_route() {
	let h = harness().await;
	for path in ["/v1/completions", "/v1/embeddings", "/v1/messages"] {
		let response = h
			.client
			.post(format!("{}{path}", h.url))
			.header("x-api-key", KEY)
			.json(&chat("qwen", false))
			.send()
			.await
			.unwrap();
		assert_eq!(response.status(), StatusCode::OK, "{path}");
	}
	assert!(
		h.seen
			.lock()
			.unwrap()
			.headers
			.iter()
			.all(|h| h.get("x-api-key").is_none())
	);
	// The messages route answers with Anthropic's usage: 4 + 6 cached in, 5 out.
	assert_eq!(h.usage(3).await, (3, 7 + 7 + 10, 3 + 3 + 5));
}

#[tokio::test]
async fn gateway_streams_the_first_byte_before_the_engine_finishes() {
	let h = harness().await;
	let started = Instant::now();
	let mut response = h
		.post("/v1/chat/completions", &chat("qwen", true))
		.send()
		.await
		.unwrap();
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(
		response.headers()[header::CONTENT_TYPE],
		"text/event-stream"
	);
	let first = response.chunk().await.unwrap().unwrap();
	let first_at = started.elapsed();
	assert!(String::from_utf8_lossy(&first).contains("\"he\""));
	assert!(
		first_at < SLOW / 2,
		"first byte after {first_at:?}; the engine needs {SLOW:?} to finish"
	);

	let mut rest = String::new();
	while let Some(chunk) = response.chunk().await.unwrap() {
		rest.push_str(&String::from_utf8_lossy(&chunk));
	}
	assert!(started.elapsed() >= SLOW, "the stream ended early");
	assert!(rest.contains("[DONE]"));

	// The gateway asked for the usage chunk, and counted it.
	assert_eq!(
		h.seen.lock().unwrap().bodies[0]["stream_options"],
		json!({"include_usage": true})
	);
	assert_eq!(h.usage(1).await, (1, 7, 3));
}

#[tokio::test]
async fn gateway_leaves_stream_options_alone_when_the_engine_cannot_take_them() {
	let h = harness().await;
	let response = h
		.post("/v1/chat/completions", &chat("plain", true))
		.send()
		.await
		.unwrap();
	let text = response.text().await.unwrap();
	assert!(text.contains("[DONE]"));
	assert!(
		h.seen.lock().unwrap().bodies[0]
			.get("stream_options")
			.is_none()
	);
	// No usage chunk came, so the request is counted without tokens.
	assert_eq!(h.usage(1).await, (1, 0, 0));
}

#[tokio::test]
async fn gateway_keeps_the_stream_options_the_client_chose() {
	let h = harness().await;
	let mut body = chat("qwen", true);
	body["stream_options"] = json!({"include_usage": false});
	h.post("/v1/chat/completions", &body)
		.send()
		.await
		.unwrap()
		.text()
		.await
		.unwrap();
	assert_eq!(
		h.seen.lock().unwrap().bodies[0]["stream_options"],
		json!({"include_usage": false})
	);
}

#[tokio::test]
async fn gateway_counts_an_anthropic_stream() {
	let h = harness().await;
	let started = Instant::now();
	let mut response = h
		.client
		.post(format!("{}/v1/messages", h.url))
		.header("x-api-key", KEY)
		.json(&chat("qwen", true))
		.send()
		.await
		.unwrap();
	let first = response.chunk().await.unwrap().unwrap();
	assert!(String::from_utf8_lossy(&first).contains("message_start"));
	assert!(started.elapsed() < SLOW / 2);
	while response.chunk().await.unwrap().is_some() {}

	// Anthropic requests are not given `stream_options`.
	assert!(
		h.seen.lock().unwrap().bodies[0]
			.get("stream_options")
			.is_none()
	);
	assert_eq!(h.usage(1).await, (1, 12, 9));
}

#[tokio::test]
async fn gateway_an_engine_that_is_down_is_502() {
	let h = harness().await;
	h.gateway.set_config(GatewayConfig {
		keys: vec![GatewayKey {
			key_hash: hash_key(KEY),
			// Nothing listens on port 1.
			models: vec![model(1, "qwen", 1, true)],
		}],
		other_models: Vec::new(),
	});
	let response = h
		.post("/v1/chat/completions", &chat("qwen", false))
		.send()
		.await
		.unwrap();
	assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn gateway_usage_is_kept_per_key_model_and_hour() {
	let h = harness().await;
	h.post("/v1/chat/completions", &chat("qwen", false))
		.send()
		.await
		.unwrap();
	h.post("/v1/chat/completions", &chat("plain", false))
		.send()
		.await
		.unwrap();
	let mut rows = Vec::new();
	for _ in 0..100 {
		rows.extend(h.gateway.take_usage());
		if rows.len() >= 2 {
			break;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	rows.sort_by_key(|r| r.model_id);
	assert_eq!(rows.len(), 2);
	assert_eq!(rows[0].model_id, 1);
	assert_eq!(rows[0].key_hash, hash_key(KEY));
	assert_eq!(rows[0].hour % 3600, 0);
	assert_eq!(rows[1].model_id, 2);
	assert!(
		h.gateway.take_usage().is_empty(),
		"taking resets the counters"
	);
}
