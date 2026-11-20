//! The bridge against a stub stdio MCP server: a shell loop that answers
//! `initialize` and `tools/list` and counts what it was sent.

use serde_json::{Value, json};

use super::*;

/// Answers by method name, echoing the id it was sent. `initialize` and
/// `notifications/initialized` are appended to `$LOG` so a test can see what
/// reached the child.
const STUB: &str = r#"
while IFS= read -r line; do
	id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
	case "$line" in
		*'"method":"initialize"'*)
			echo initialize >> "$LOG"
			printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"stub","version":"1"}}}\n' "$id";;
		*'"method":"notifications/initialized"'*)
			echo initialized >> "$LOG";;
		*'"method":"tools/list"'*)
			printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echoes","inputSchema":{"type":"object"}}]}}\n' "$id";;
		*'"method":"silent"'*) ;;
		*)
			printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"no"}}\n' "$id";;
	esac
done
"#;

struct Running {
	url: String,
	log: std::path::PathBuf,
	exited: oneshot::Receiver<String>,
	_dir: tempfile::TempDir,
}

async fn start(timeout: Duration) -> Running {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let dir = tempfile::tempdir().unwrap();
	let log = dir.path().join("log");
	let script = dir.path().join("stub.sh");
	std::fs::write(&script, STUB).unwrap();
	let command = vec![
		"env".to_owned(),
		format!("LOG={}", log.display()),
		"sh".to_owned(),
		script.display().to_string(),
	];
	let started = Bridge::spawn(&command, timeout).unwrap();
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}/mcp", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, router(started.bridge, "/mcp"))
			.await
			.unwrap();
	});
	Running {
		url,
		log,
		exited: started.exited,
		_dir: dir,
	}
}

fn rpc(id: u64, method: &str) -> Value {
	json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": {} })
}

async fn post(url: &str, body: &Value) -> (reqwest::StatusCode, Value) {
	let response = reqwest::Client::new()
		.post(url)
		.header("accept", "application/json, text/event-stream")
		.json(body)
		.send()
		.await
		.unwrap();
	let status = response.status();
	let text = response.text().await.unwrap();
	(status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn log_of(running: &Running) -> Vec<String> {
	std::fs::read_to_string(&running.log)
		.unwrap_or_default()
		.lines()
		.map(str::to_owned)
		.collect()
}

#[tokio::test]
async fn mcp_resource_bridge_round_trip() {
	let running = start(Duration::from_secs(10)).await;

	let (status, init) = post(&running.url, &rpc(7, "initialize")).await;
	assert_eq!(status, 200);
	assert_eq!(init["id"], 7, "the client's id comes back");
	assert_eq!(init["result"]["serverInfo"]["name"], "stub");

	let (status, _) = post(
		&running.url,
		&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
	)
	.await;
	assert_eq!(status, 202, "a notification has no answer");

	let (status, tools) = post(&running.url, &rpc(8, "tools/list")).await;
	assert_eq!(status, 200);
	assert_eq!(tools["id"], 8);
	assert_eq!(tools["result"]["tools"][0]["name"], "echo");
}

#[tokio::test]
async fn mcp_resource_bridge_clients_share_one_handshake_and_keep_their_ids() {
	let running = start(Duration::from_secs(10)).await;
	let (_, first) = post(&running.url, &rpc(1, "initialize")).await;
	let (_, second) = post(&running.url, &rpc(1, "initialize")).await;
	assert_eq!(first["result"], second["result"]);
	for _ in 0..2 {
		post(
			&running.url,
			&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
		)
		.await;
	}
	// Two clients both using id 1 at once: each gets its own answer, under id 1.
	let list = rpc(1, "tools/list");
	let (a, b) = tokio::join!(post(&running.url, &list), post(&running.url, &list));
	assert_eq!(a.1["id"], 1);
	assert_eq!(b.1["id"], 1);
	assert_eq!(a.1["result"]["tools"][0]["name"], "echo");
	assert_eq!(b.1["result"]["tools"][0]["name"], "echo");
	assert_eq!(
		log_of(&running),
		["initialize", "initialized"],
		"the child saw one handshake"
	);
}

#[tokio::test]
async fn mcp_resource_bridge_batches_unknown_methods_and_bad_bodies() {
	let running = start(Duration::from_secs(10)).await;
	let (status, answers) = post(
		&running.url,
		&json!([rpc(1, "tools/list"), rpc(2, "nope"), { "jsonrpc": "2.0", "method": "x" }]),
	)
	.await;
	assert_eq!(status, 200);
	assert_eq!(answers.as_array().unwrap().len(), 2);
	assert_eq!(answers[1]["error"]["code"], -32601);

	let response = reqwest::Client::new()
		.post(&running.url)
		.body("not json")
		.send()
		.await
		.unwrap();
	assert_eq!(response.status(), 400);

	for method in [reqwest::Method::GET, reqwest::Method::DELETE] {
		let response = reqwest::Client::new()
			.request(method, &running.url)
			.send()
			.await
			.unwrap();
		assert_eq!(response.status(), 405);
		assert_eq!(response.headers()["allow"], "POST");
	}
	let health = reqwest::get(running.url.replace("/mcp", "/healthz"))
		.await
		.unwrap();
	assert_eq!(health.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn mcp_resource_bridge_gives_up_on_a_silent_server_and_reports_its_exit() {
	let running = start(Duration::from_millis(300)).await;
	let (status, answer) = post(&running.url, &rpc(3, "silent")).await;
	assert_eq!(status, 200);
	assert_eq!(answer["id"], 3);
	assert_eq!(answer["error"]["code"], INTERNAL_ERROR);

	let gone = Bridge::spawn(&["true".to_owned()], Duration::from_secs(1)).unwrap();
	let message = tokio::time::timeout(Duration::from_secs(5), gone.exited)
		.await
		.unwrap()
		.unwrap();
	assert!(message.contains("exited"), "{message}");
	assert!(Bridge::spawn(&[], Duration::from_secs(1)).is_err());
	assert!(Bridge::spawn(&["/nonexistent/podkit".to_owned()], Duration::from_secs(1)).is_err());
	drop(running.exited);
}
