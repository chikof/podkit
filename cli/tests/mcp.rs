//! The stdio relay against a stand-in server, and the live test against a real one.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use cli::{Target, relay};
use serde_json::{Value, json};

type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// A server that answers `ping` with `{}`, treats a message without an id as a
/// notification, and rejects the token `bad`. It records what it was sent.
async fn stand_in() -> (String, Seen) {
	async fn mcp(State(seen): State<Seen>, headers: HeaderMap, body: String) -> impl IntoResponse {
		let auth = headers
			.get(header::AUTHORIZATION)
			.and_then(|v| v.to_str().ok())
			.unwrap_or_default()
			.to_owned();
		seen.lock().unwrap().push((auth.clone(), body.clone()));
		if auth == "Bearer bad" {
			return (
				StatusCode::UNAUTHORIZED,
				r#"{"error":"invalid token"}"#.to_owned(),
			);
		}
		let message: Value = serde_json::from_str(&body).unwrap();
		match message.get("id") {
			None => (StatusCode::ACCEPTED, String::new()),
			// Pretty-printed on purpose: the relay must still write one line.
			Some(id) => (
				StatusCode::OK,
				serde_json::to_string_pretty(&json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
					.unwrap(),
			),
		}
	}
	let seen = Seen::default();
	let app = Router::new()
		.route("/mcp", post(mcp))
		.with_state(seen.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(format!("http://{addr}/"), seen)
}

async fn run(base: &str, token: &str, input: &str) -> Vec<Value> {
	let mut out = Vec::new();
	relay(
		input.as_bytes(),
		&mut out,
		&cli::client(),
		&Target::new(base, token),
	)
	.await
	.unwrap();
	String::from_utf8(out)
		.unwrap()
		.lines()
		.map(|l| serde_json::from_str(l).unwrap())
		.collect()
}

#[tokio::test]
async fn mcp_relay_sends_requests_with_the_token_and_writes_one_line_back() {
	let (base, seen) = stand_in().await;
	let input = concat!(
		r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
		"\n\n",
		r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
		"\n",
		r#"{"jsonrpc":"2.0","id":"two","method":"ping"}"#,
		"\n",
	);
	let replies = run(&base, "pk_good", input).await;
	assert_eq!(
		replies.len(),
		2,
		"the notification gets no reply: {replies:?}"
	);
	assert_eq!(replies[0]["id"], 1);
	assert_eq!(replies[1]["id"], "two");
	let seen = seen.lock().unwrap();
	assert_eq!(seen.len(), 3, "the blank line is skipped");
	assert!(seen.iter().all(|(auth, _)| auth == "Bearer pk_good"));
}

#[tokio::test]
async fn mcp_relay_turns_a_refusal_into_a_json_rpc_error() {
	let (base, _) = stand_in().await;
	let replies = run(
		&base,
		"bad",
		"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
	)
	.await;
	assert_eq!(replies[0]["id"], 7);
	let message = replies[0]["error"]["message"].as_str().unwrap();
	assert!(
		message.contains("401") && message.contains("invalid token"),
		"{message}"
	);
	// A notification that is refused has nobody to tell: stdout stays empty.
	let replies = run(&base, "bad", "{\"jsonrpc\":\"2.0\",\"method\":\"x\"}\n").await;
	assert!(replies.is_empty());
}

#[tokio::test]
async fn mcp_relay_answers_when_the_server_is_down() {
	let replies = run(
		"http://127.0.0.1:1",
		"pk_x",
		"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n",
	)
	.await;
	assert_eq!(replies[0]["id"], 3);
	assert!(
		replies[0]["error"]["message"]
			.as_str()
			.unwrap()
			.contains("could not reach podkit")
	);
}

/// Drives the real `podkit mcp` binary against a running server, as an MCP client does:
/// initialize, list the tools, call `list_projects`.
///
/// Run: `PODKIT_LIVE_URL=http://127.0.0.1:8080 PODKIT_LIVE_TOKEN=pk_… \
///   cargo test -p cli -- --ignored mcp_live`
#[tokio::test]
#[ignore = "needs a running podkit and an API token"]
async fn mcp_live_stdio_lists_projects() {
	use std::process::Stdio;

	use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

	let url = std::env::var("PODKIT_LIVE_URL").expect("PODKIT_LIVE_URL");
	let token = std::env::var("PODKIT_LIVE_TOKEN").expect("PODKIT_LIVE_TOKEN");
	let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_podkit"))
		.arg("mcp")
		.env("PODKIT_URL", url)
		.env("PODKIT_TOKEN", token)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.spawn()
		.unwrap();
	let mut stdin = child.stdin.take().unwrap();
	let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();

	let mut ask = async |message: Value| {
		stdin
			.write_all(format!("{message}\n").as_bytes())
			.await
			.unwrap();
		let line = stdout.next_line().await.unwrap().expect("a reply");
		serde_json::from_str::<Value>(&line).unwrap()
	};
	let init = ask(json!({
		"jsonrpc": "2.0", "id": 1, "method": "initialize",
		"params": {
			"protocolVersion": "2025-06-18",
			"capabilities": {},
			"clientInfo": { "name": "podkit-test", "version": "0" },
		},
	}))
	.await;
	assert_eq!(init["result"]["serverInfo"]["name"], "podkit", "{init}");

	let tools = ask(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await;
	let names: Vec<&str> = tools["result"]["tools"]
		.as_array()
		.unwrap()
		.iter()
		.map(|t| t["name"].as_str().unwrap())
		.collect();
	assert!(names.contains(&"list_projects"), "{names:?}");

	let called = ask(json!({
		"jsonrpc": "2.0", "id": 3, "method": "tools/call",
		"params": { "name": "list_projects", "arguments": {} },
	}))
	.await;
	assert_ne!(called["result"]["isError"], json!(true), "{called}");
	assert!(
		called["result"]["content"][0]["text"].is_string(),
		"{called}"
	);

	drop(stdin);
	let _ = child.wait().await;
}
