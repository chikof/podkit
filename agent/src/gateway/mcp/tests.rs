//! Gateway auth, forwarding and counting for `/mcp/<name>`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::any;
use proto::{GatewayConfig, GatewayMcp, GatewayMcpKey};
use serde_json::{Value, json};

use super::*;
use crate::gateway::router;

const KEY_FILES: &str = "pk-link-files";
const KEY_FETCH: &str = "pk-link-fetch";

#[derive(Default)]
struct Seen {
	requests: Vec<(Method, Uri, HeaderMap, Bytes)>,
}

type Stub = Arc<Mutex<Seen>>;

async fn upstream(
	State(seen): State<Stub>,
	method: Method,
	uri: Uri,
	headers: HeaderMap,
	body: Bytes,
) -> axum::response::Response {
	seen.lock()
		.unwrap()
		.requests
		.push((method, uri, headers, body));
	(
		StatusCode::OK,
		[
			("content-type", "application/json"),
			("mcp-session-id", "s1"),
		],
		json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}).to_string(),
	)
		.into_response()
}

async fn serve(app: Router) -> u16 {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	port
}

fn mcp(id: i64, name: &str, port: u16) -> GatewayMcp {
	GatewayMcp {
		server_id: id,
		name: name.to_owned(),
		host: "127.0.0.1".to_owned(),
		port,
		path: "/mcp".to_owned(),
	}
}

struct Harness {
	gateway: Arc<Gateway>,
	url: String,
	seen: Stub,
}

async fn harness() -> Harness {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let seen = Stub::default();
	let port = serve(
		Router::new()
			.route("/mcp", any(upstream))
			.route("/mcp/{*rest}", any(upstream))
			.with_state(seen.clone()),
	)
	.await;
	let gateway = Gateway::new().unwrap();
	gateway.set_config(GatewayConfig {
		mcp_keys: vec![
			GatewayMcpKey {
				key_hash: hash_key(KEY_FILES),
				mcp: mcp(10, "files", port),
			},
			GatewayMcpKey {
				key_hash: hash_key(KEY_FETCH),
				mcp: mcp(11, "fetch", port),
			},
		],
		other_mcp: vec!["secret".to_owned()],
		..Default::default()
	});
	let url = format!("http://{}", {
		let app = router(gateway.clone());
		format!("127.0.0.1:{}", serve(app).await)
	});
	Harness { gateway, url, seen }
}

async fn call(h: &Harness, path: &str, key: Option<&str>) -> reqwest::Response {
	let mut request = reqwest::Client::new()
		.post(format!("{}{path}", h.url))
		.header("content-type", "application/json")
		.body(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string());
	if let Some(key) = key {
		request = request.bearer_auth(key);
	}
	request.send().await.unwrap()
}

#[tokio::test]
async fn mcp_resource_gateway_refuses_a_missing_wrong_or_foreign_key() {
	let h = harness().await;
	assert_eq!(call(&h, "/mcp/files", None).await.status(), 401);
	assert_eq!(call(&h, "/mcp/files", Some("pk-wrong")).await.status(), 401);
	// A key of the model gateway or of any other kind is just as unknown.
	assert_eq!(
		call(&h, "/mcp/fetch", Some(KEY_FILES)).await.status(),
		403,
		"another link's server"
	);
	assert_eq!(call(&h, "/mcp/secret", Some(KEY_FILES)).await.status(), 403);
	assert_eq!(call(&h, "/mcp/nope", Some(KEY_FILES)).await.status(), 404);
	assert_eq!(call(&h, "/mcp/", Some(KEY_FILES)).await.status(), 404);
	assert!(h.seen.lock().unwrap().requests.is_empty());
	assert!(
		h.gateway.take_usage().is_empty(),
		"refused requests are not counted"
	);

	let body: Value = call(&h, "/mcp/fetch", Some(KEY_FILES))
		.await
		.json()
		.await
		.unwrap();
	assert_eq!(body["type"], "error");
	assert!(!body.to_string().contains(KEY_FILES));
}

#[tokio::test]
async fn mcp_resource_gateway_forwards_without_credentials_and_counts() {
	let h = harness().await;
	let response = reqwest::Client::new()
		.post(format!("{}/mcp/files?a=1", h.url))
		.bearer_auth(KEY_FILES)
		.header("x-api-key", KEY_FILES)
		.header("mcp-session-id", "abc")
		.header("accept", "application/json, text/event-stream")
		.body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
		.send()
		.await
		.unwrap();
	assert_eq!(response.status(), 200);
	assert_eq!(response.headers()["mcp-session-id"], "s1");
	let body: Value = response.json().await.unwrap();
	assert_eq!(body["result"]["tools"], json!([]));

	let rest = call(&h, "/mcp/files/extra", Some(KEY_FILES)).await;
	assert_eq!(rest.status(), 200);

	{
		let seen = h.seen.lock().unwrap();
		assert_eq!(seen.requests.len(), 2);
		let (method, uri, headers, body) = &seen.requests[0];
		assert_eq!(method, Method::POST);
		assert_eq!(uri.to_string(), "/mcp?a=1");
		assert!(headers.get("authorization").is_none(), "{headers:?}");
		assert!(headers.get("x-api-key").is_none());
		assert_eq!(headers["mcp-session-id"], "abc");
		assert_eq!(
			headers["accept"], "application/json, text/event-stream",
			"the client's own headers are kept"
		);
		assert!(String::from_utf8_lossy(body).contains("tools/list"));
		assert_eq!(seen.requests[1].1.to_string(), "/mcp/extra");
	}

	let usage = h.gateway.take_usage();
	assert_eq!(usage.len(), 1);
	assert_eq!(usage[0].key_hash, hash_key(KEY_FILES));
	assert_eq!(usage[0].model_id, 10);
	assert!(usage[0].mcp);
	assert_eq!(usage[0].requests, 2);
	assert_eq!((usage[0].input_tokens, usage[0].output_tokens), (0, 0));
	assert!(h.gateway.take_usage().is_empty());
}

#[tokio::test]
async fn mcp_resource_gateway_counts_per_key_and_reports_a_dead_server() {
	let h = harness().await;
	call(&h, "/mcp/files", Some(KEY_FILES)).await;
	call(&h, "/mcp/fetch", Some(KEY_FETCH)).await;
	call(&h, "/mcp/fetch", Some(KEY_FETCH)).await;
	let mut usage = h.gateway.take_usage();
	usage.sort_by_key(|u| u.model_id);
	assert_eq!(
		usage
			.iter()
			.map(|u| (u.model_id, u.requests))
			.collect::<Vec<_>>(),
		[(10, 1), (11, 2)]
	);

	// Nothing listens on the port the next config names.
	let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let dead_port = dead.local_addr().unwrap().port();
	drop(dead);
	h.gateway.set_config(GatewayConfig {
		mcp_keys: vec![GatewayMcpKey {
			key_hash: hash_key(KEY_FILES),
			mcp: mcp(10, "files", dead_port),
		}],
		..Default::default()
	});
	assert_eq!(call(&h, "/mcp/files", Some(KEY_FILES)).await.status(), 502);
	assert!(h.gateway.take_usage().is_empty());
}

#[test]
fn mcp_resource_gateway_paths_split_into_name_and_rest() {
	assert_eq!(split("/mcp/files"), Some(("files", "")));
	assert_eq!(split("/mcp/files/a/b"), Some(("files", "/a/b")));
	assert_eq!(split("/mcp//a"), None);
	assert_eq!(split("/v1/models"), None);
}

#[test]
fn mcp_resource_tools_request_is_checked_and_keeps_the_key_out_of_the_script() {
	let ok = GatewayMcpToolsRequest {
		key: "pk-secret".to_owned(),
		name: "my-files_1".to_owned(),
		timeout_s: 30,
	};
	assert!(validate(&ok).is_ok());
	for edit in [
		(|r: &mut GatewayMcpToolsRequest| r.key.clear()) as fn(&mut GatewayMcpToolsRequest),
		|r| r.key = "a\nb".to_owned(),
		|r| r.name = "a/b".to_owned(),
		|r| r.name = "a b".to_owned(),
		|r| r.name.clear(),
		|r| r.timeout_s = 0,
		|r| r.timeout_s = 9999,
	] {
		let mut req = ok.clone();
		edit(&mut req);
		assert!(validate(&req).is_err(), "{req:?}");
	}
	let spec = container_spec(&ok, "podkit-mcp-x");
	let command = spec.command.unwrap();
	assert!(!command[3].contains("pk-secret"));
	assert_eq!(command[4], "http://llm.podkit.internal:8088/mcp/my-files_1");
	assert_eq!(spec.env, [(KEY_ENV.to_owned(), "pk-secret".to_owned())]);
	assert_eq!(spec.network, "podkit");
	assert!(!format!("{ok:?}").contains("pk-secret"));
}
