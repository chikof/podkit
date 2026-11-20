//! MCP server resource tests against a fake agent on the local server's socket.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{post, put};
use proto::{GatewayConfig, GatewayUsage};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::tests::{call, state, user_with_team};

#[derive(Default)]
struct FakeAgent {
	deploys: Mutex<Vec<Value>>,
	configs: Mutex<Vec<GatewayConfig>>,
	/// What the next drain returns.
	usage: Mutex<Vec<GatewayUsage>>,
	/// What the tools probe prints.
	tools_line: Mutex<String>,
	tools_requests: Mutex<Vec<Value>>,
}

fn ndjson(events: &[Value]) -> axum::response::Response {
	let body = events.iter().fold(String::new(), |mut out, e| {
		out.push_str(&e.to_string());
		out.push('\n');
		out
	});
	([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	let (deploy, config, drain, tools) =
		(agent.clone(), agent.clone(), agent.clone(), agent.clone());
	let app = Router::new()
		.route(
			"/v1/deploy",
			post(move |body: Bytes| async move {
				deploy
					.deploys
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				ndjson(&[
					json!({ "type": "log", "stream": "system", "line": "deploying" }),
					json!({ "type": "deployed", "container_id": "c1", "image": "x",
						"commit_sha": null }),
				])
			}),
		)
		.route(
			"/v1/gateway/config",
			put(move |body: Bytes| async move {
				config
					.configs
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				axum::Json(json!({}))
			}),
		)
		.route(
			"/v1/gateway/usage/drain",
			post(
				move || async move { axum::Json(std::mem::take(&mut *drain.usage.lock().unwrap())) },
			),
		)
		.route(
			"/v1/gateway/mcp/tools",
			post(move |body: Bytes| async move {
				tools
					.tools_requests
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				let line = tools.tools_line.lock().unwrap().clone();
				ndjson(&[
					json!({ "type": "log", "stream": "stdout", "line": line }),
					json!({ "type": "done" }),
				])
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	agent
}

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	agent: Arc<FakeAgent>,
	token: String,
	team: i64,
	project: i64,
	server: i64,
	/// An ordinary application that links to MCP servers.
	consumer: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let agent = fake_agent(&state);
		Self::on(db, state, agent).await
	}

	/// The same project on `state`, whatever answers on its agent socket.
	async fn on(db: PgPool, state: AppState, agent: Arc<FakeAgent>) -> Self {
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (project, server) = Self::project_and_server(&app, &token, team).await;
		let (status, consumer) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(&token),
			Some(json!({
				"name": "agent", "slug": "agent", "server_id": server,
				"repo_url": "https://github.com/acme/agent", "git_ref": "main",
				"container_port": 3000,
			})),
		)
		.await;
		assert!(status.is_success(), "{consumer}");
		Self {
			app,
			state,
			db,
			agent,
			token,
			team,
			project,
			server,
			consumer: consumer["id"].as_i64().unwrap(),
		}
	}

	async fn project_and_server(app: &Router, token: &str, team: i64) -> (i64, i64) {
		let (_, servers) = call(
			app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(token),
			None,
		)
		.await;
		let (status, project) = call(
			app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(token),
			Some(json!({ "name": "tools", "slug": "tools" })),
		)
		.await;
		assert!(status.is_success(), "{project}");
		(
			project["id"].as_i64().unwrap(),
			servers[0]["id"].as_i64().unwrap(),
		)
	}

	fn base(&self) -> String {
		format!("/api/v1/teams/{}/projects/{}", self.team, self.project)
	}

	async fn api(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(
			&self.app,
			method,
			&format!("{}{path}", self.base()),
			Some(&self.token),
			body,
		)
		.await
	}

	/// An MCP server from an image, on `server`.
	async fn server_on(&self, server: i64, name: &str, transport: &str) -> (StatusCode, Value) {
		let mut body = json!({
			"name": name, "server_id": server,
			"image": "ghcr.io/acme/mcp-files:1", "transport": transport,
		});
		if transport == "stdio" {
			body["command"] = json!(["node", "dist/index.js", "/data"]);
		}
		self.api("POST", "/mcp-servers", Some(body)).await
	}

	async fn server(&self, name: &str, transport: &str) -> (StatusCode, Value) {
		self.server_on(self.server, name, transport).await
	}

	async fn mcp(&self, name: &str) -> i64 {
		let (status, body) = self.server(name, "http").await;
		assert_eq!(status, StatusCode::CREATED, "{body}");
		body["id"].as_i64().unwrap()
	}

	async fn link(&self, mcp: i64) -> (StatusCode, Value) {
		self.api(
			"POST",
			&format!("/applications/{}/mcp", self.consumer),
			Some(json!({ "mcp_server_id": mcp })),
		)
		.await
	}

	/// Deploys an application and returns the spec the agent was asked to run.
	async fn deploy(&self, application: i64) -> Value {
		let (status, deployment) = self
			.api(
				"POST",
				&format!("/applications/{application}/deployments"),
				None,
			)
			.await;
		assert_eq!(status, StatusCode::ACCEPTED, "{deployment}");
		let id = deployment["id"].as_i64().unwrap();
		let job = jobs::latest_for(&self.db, JobKind::Deploy, id)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		crate::deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log)
			.await
			.unwrap();
		log.finish().await;
		self.agent.deploys.lock().unwrap().last().unwrap().clone()
	}

	fn last_config(&self) -> GatewayConfig {
		self.agent
			.configs
			.lock()
			.unwrap()
			.last()
			.expect("the table was pushed")
			.clone()
	}
}

fn env_of(spec: &Value) -> Vec<(String, String)> {
	serde_json::from_value(spec["env"].clone()).unwrap()
}

fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
	env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[sqlx::test]
async fn mcp_resource_create_deploys_on_the_environment_network_and_never_publicly(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, created) = s.server("Files", "http").await;
	assert_eq!(status, StatusCode::CREATED, "{created}");
	assert_eq!(created["slug"], "files");
	assert_eq!(created["transport"], "http");
	assert_eq!(created["path"], "/mcp");
	assert_eq!(
		created["gateway_url"],
		"http://llm.podkit.internal:8088/mcp/files"
	);
	let id = created["id"].as_i64().unwrap();

	let spec = s.deploy(id).await;
	assert!(
		spec["routing"].is_null(),
		"an MCP server is not routed publicly"
	);
	assert_eq!(spec["attach"][0]["aliases"], json!(["mcp-files"]));
	assert!(spec["options"].get("mcp_bridge").is_none());

	// A stdio server runs behind the agent's bridge on its own port.
	let (status, stdio) = s.server("Local", "stdio").await;
	assert_eq!(status, StatusCode::CREATED, "{stdio}");
	let spec = s.deploy(stdio["id"].as_i64().unwrap()).await;
	assert_eq!(
		spec["options"]["mcp_bridge"],
		json!({ "port": 8080, "path": "/mcp", "command": ["node", "dist/index.js", "/data"] })
	);

	// Listed, shown, updated.
	let (_, list) = s.api("GET", "/mcp-servers", None).await;
	assert_eq!(list.as_array().unwrap().len(), 2);
	let (status, shown) = s.api("GET", &format!("/mcp-servers/{id}"), None).await;
	assert_eq!(
		(status, shown["running"].clone()),
		(StatusCode::OK, json!(true))
	);
	let (status, updated) = s
		.api(
			"PUT",
			&format!("/mcp-servers/{id}"),
			Some(json!({ "path": "/rpc" })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{updated}");
	assert_eq!(updated["path"], "/rpc");
}

#[sqlx::test]
async fn mcp_resource_create_refuses_bad_input(db: PgPool) {
	let s = Setup::new(db).await;
	for body in [
		json!({ "name": "x", "server_id": s.server, "transport": "http" }),
		json!({ "name": "x", "server_id": s.server, "transport": "http",
			"image": "a/b:1", "repo_url": "https://example.com/r" }),
		json!({ "name": "x", "server_id": s.server, "transport": "carrier-pigeon", "image": "a/b:1" }),
		json!({ "name": "x", "server_id": s.server, "transport": "http", "image": "a/b:1",
			"command": ["node"] }),
		json!({ "name": "x", "server_id": s.server, "transport": "http", "image": "a/b:1",
			"path": "no-slash" }),
	] {
		let (status, body) = s.api("POST", "/mcp-servers", Some(body)).await;
		assert!(
			status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
			"{status} {body}"
		);
	}
	let (_, apps) = s.api("GET", "/applications", None).await;
	assert_eq!(
		apps.as_array().unwrap().len(),
		1,
		"no half-made application is left"
	);
}

#[sqlx::test]
async fn mcp_resource_link_env_injection_gives_each_link_its_own_key(db: PgPool) {
	let s = Setup::new(db).await;
	let (files, fetch) = (s.mcp("Files").await, s.mcp("Fetch").await);

	let (status, link) = s.link(files).await;
	assert_eq!(status, StatusCode::CREATED, "{link}");
	assert_eq!(link["slug"], "files");
	assert_eq!(link["url_variable"], "MCP_FILES_URL");
	assert!(
		!link.to_string().contains("pk-"),
		"the key is never returned: {link}"
	);
	assert_eq!(s.link(fetch).await.0, StatusCode::CREATED);
	// Linking again is not an error and issues no second key.
	assert_eq!(s.link(files).await.0, StatusCode::CREATED);

	let config = s.last_config();
	assert_eq!(config.mcp_keys.len(), 2);
	assert_ne!(config.mcp_keys[0].key_hash, config.mcp_keys[1].key_hash);
	let files_key = config
		.mcp_keys
		.iter()
		.find(|k| k.mcp.name == "files")
		.unwrap();
	assert_eq!(files_key.mcp.host, "mcp-files");
	assert_eq!(files_key.mcp.port, 8080);
	assert_eq!(files_key.mcp.path, "/mcp");
	assert_eq!(files_key.mcp.server_id, files);
	assert_eq!(config.other_mcp, ["fetch", "files"]);
	assert_eq!(config.networks.len(), 1, "one environment, one network");

	let env = env_of(&s.deploy(s.consumer).await);
	assert_eq!(
		value(&env, "MCP_FILES_URL"),
		Some("http://llm.podkit.internal:8088/mcp/files")
	);
	let key = value(&env, "MCP_FILES_KEY").unwrap();
	assert_eq!(crate::links::hash_key(key), files_key.key_hash);
	assert_ne!(value(&env, "MCP_FETCH_KEY"), Some(key), "a key per link");
	let list: Value = serde_json::from_str(value(&env, "PODKIT_MCP_SERVERS").unwrap()).unwrap();
	assert_eq!(list.as_array().unwrap().len(), 2);
	assert_eq!(list[0]["name"], "fetch");
	assert_eq!(list[1]["name"], "files");
	assert_eq!(list[1]["url"], "http://llm.podkit.internal:8088/mcp/files");
	assert_eq!(list[1]["key"], key);
	assert!(value(&env, "OPENAI_API_KEY").is_none());

	// A user's variable of the same name loses.
	let (status, _) = s
		.api(
			"POST",
			&format!("/applications/{}/env-vars", s.consumer),
			Some(json!({ "key": "MCP_FILES_URL", "value": "http://evil" })),
		)
		.await;
	assert!(status.is_success());
	let env = env_of(&s.deploy(s.consumer).await);
	assert_eq!(
		value(&env, "MCP_FILES_URL"),
		Some("http://llm.podkit.internal:8088/mcp/files")
	);

	// Unlinking removes the key from the gateway at once and the variables on the next deploy.
	let (status, _) = s
		.api(
			"DELETE",
			&format!("/applications/{}/mcp/{files}", s.consumer),
			None,
		)
		.await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert_eq!(s.last_config().mcp_keys.len(), 1);
	let env = env_of(&s.deploy(s.consumer).await);
	// Only the user's own variable is left.
	assert_eq!(value(&env, "MCP_FILES_URL"), Some("http://evil"));
	let list: Value = serde_json::from_str(value(&env, "PODKIT_MCP_SERVERS").unwrap()).unwrap();
	assert_eq!(list.as_array().unwrap().len(), 1);
	let (status, _) = s
		.api(
			"DELETE",
			&format!("/applications/{}/mcp/{files}", s.consumer),
			None,
		)
		.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn mcp_resource_link_rules(db: PgPool) {
	let s = Setup::new(db).await;
	let files = s.mcp("Files").await;

	// An application cannot be linked to itself or to something that is not an MCP server.
	let (status, _) = s
		.api(
			"POST",
			&format!("/applications/{files}/mcp"),
			Some(json!({ "mcp_server_id": files })),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(s.link(s.consumer).await.0, StatusCode::NOT_FOUND);

	// An MCP server on another server is out of the gateway's reach.
	let other_server = crypto::generate_id();
	let other_key = crate::tests::seed_key(&s.db, s.team, "remote key").await;
	sqlx::query(
		"INSERT INTO servers (id, team_id, name, hostname, ssh_port, ssh_user, ssh_key_id,
			podman_socket_path, is_local, status)
		VALUES ($1, $2, 'remote', 'remote.example', 22, 'root', $3,
			'/run/podman.sock', FALSE, 'active')",
	)
	.bind(other_server)
	.bind(s.team)
	.bind(other_key)
	.execute(&s.db)
	.await
	.unwrap();
	let (status, remote) = s.server_on(other_server, "Remote", "http").await;
	assert_eq!(status, StatusCode::CREATED, "{remote}");
	let (status, body) = s.link(remote["id"].as_i64().unwrap()).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

	// A linked server cannot be removed, until it is unlinked.
	assert_eq!(s.link(files).await.0, StatusCode::CREATED);
	let (status, body) = s
		.api("DELETE", &format!("/applications/{files}"), None)
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	let (status, linked) = s
		.api("GET", &format!("/mcp-servers/{files}/links"), None)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(linked[0]["name"], "agent");
	s.api(
		"DELETE",
		&format!("/applications/{}/mcp/{files}", s.consumer),
		None,
	)
	.await;
	let (status, body) = s
		.api("DELETE", &format!("/applications/{files}"), None)
		.await;
	assert!(status.is_success(), "{status} {body}");
}

#[sqlx::test]
async fn mcp_resource_other_teams_and_anonymous_callers_get_nothing(db: PgPool) {
	let s = Setup::new(db).await;
	let files = s.mcp("Files").await;
	s.link(files).await;

	let (bob_token, bob_team) = user_with_team(&s.app, "bob").await;
	let (bob_project, bob_server) = Setup::project_and_server(&s.app, &bob_token, bob_team).await;
	let bob = |path: String| format!("/api/v1/teams/{bob_team}/projects/{bob_project}{path}");
	let theirs = format!("/api/v1/teams/{}/projects/{}", s.team, s.project);

	// Bob names alice's ids in his own team and project, or in hers: 404 or 403, never a leak.
	for (method, path, body) in [
		("GET", bob(format!("/mcp-servers/{files}")), None),
		(
			"PUT",
			bob(format!("/mcp-servers/{files}")),
			Some(json!({"path": "/x"})),
		),
		("POST", bob(format!("/mcp-servers/{files}/tools")), None),
		("GET", bob(format!("/mcp-servers/{files}/links")), None),
		("GET", bob(format!("/mcp-servers/{files}/usage")), None),
		(
			"GET",
			bob(format!("/applications/{}/mcp", s.consumer)),
			None,
		),
		(
			"POST",
			bob(format!("/applications/{}/mcp", s.consumer)),
			Some(json!({ "mcp_server_id": files })),
		),
		(
			"DELETE",
			bob(format!("/applications/{}/mcp/{files}", s.consumer)),
			None,
		),
		("GET", format!("{theirs}/mcp-servers/{files}"), None),
		("GET", format!("{theirs}/mcp-servers"), None),
		(
			"GET",
			format!("{theirs}/applications/{}/mcp", s.consumer),
			None,
		),
	] {
		let (status, body) = call(&s.app, method, &path, Some(&bob_token), body).await;
		assert!(
			matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
			"{method} {path}: {status} {body}"
		);
		assert!(!body.to_string().contains("mcp-files"));
	}

	// His own application cannot link to her server: it is a miss.
	let (status, bob_app) = call(
		&s.app,
		"POST",
		&bob(String::from("/applications")),
		Some(&bob_token),
		Some(json!({
			"name": "mine", "slug": "mine", "server_id": bob_server,
			"repo_url": "https://github.com/bob/mine", "git_ref": "main", "container_port": 3000,
		})),
	)
	.await;
	assert!(status.is_success(), "{bob_app}");
	let (status, _) = call(
		&s.app,
		"POST",
		&bob(format!("/applications/{}/mcp", bob_app["id"])),
		Some(&bob_token),
		Some(json!({ "mcp_server_id": files })),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// No token, no answer.
	for (method, path) in [
		("GET", format!("{theirs}/mcp-servers")),
		("POST", format!("{theirs}/mcp-servers")),
		("POST", format!("{theirs}/mcp-servers/{files}/tools")),
		("POST", format!("{theirs}/applications/{}/mcp", s.consumer)),
	] {
		let body = (method == "POST").then(|| json!({}));
		let (status, _) = call(&s.app, method, &path, None, body).await;
		assert!(
			matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED),
			"{method} {path}: {status}"
		);
	}
}

#[sqlx::test]
async fn mcp_resource_usage_is_counted_per_key_and_hour_apart_from_models(db: PgPool) {
	let s = Setup::new(db).await;
	let files = s.mcp("Files").await;
	s.link(files).await;
	let hash = s.last_config().mcp_keys[0].key_hash.clone();
	let server = crate::servers::find(&s.db, crate::ids::ServerId(s.server))
		.await
		.unwrap()
		.unwrap();
	let client = s.state.agents.client(&s.state, &server).await.unwrap();
	let hour = 1_700_000_000 - 1_700_000_000 % 3600;

	for requests in [2, 3] {
		*s.agent.usage.lock().unwrap() = vec![GatewayUsage {
			key_hash: hash.clone(),
			model_id: files,
			hour,
			requests,
			input_tokens: 0,
			output_tokens: 0,
			mcp: true,
		}];
		crate::links::collect_usage(&s.state, &client)
			.await
			.unwrap();
	}
	let (rows, requests, application, team): (i64, i64, i64, i64) = sqlx::query_as(
		"SELECT COUNT(*), SUM(requests)::bigint, MIN(application_id), MIN(team_id) FROM mcp_usage",
	)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!((rows, requests), (1, 5));
	assert_eq!((application, team), (s.consumer, s.team));
	let models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_usage")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(models, 0, "an MCP count is not a model count");

	// The page's numbers cover the last 7 days: that hour in 2023 is out of range, so add one now.
	let now_hour = time::OffsetDateTime::now_utc().unix_timestamp() / 3600 * 3600;
	*s.agent.usage.lock().unwrap() = vec![GatewayUsage {
		key_hash: hash,
		model_id: files,
		hour: now_hour,
		requests: 7,
		input_tokens: 0,
		output_tokens: 0,
		mcp: true,
	}];
	crate::links::collect_usage(&s.state, &client)
		.await
		.unwrap();
	let (status, usage) = s
		.api("GET", &format!("/mcp-servers/{files}/usage"), None)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(usage[0]["key"], "agent");
	assert_eq!(usage[0]["requests_24h"], 7);
	assert_eq!(usage[0]["requests_7d"], 7);
}

#[sqlx::test]
async fn mcp_resource_tools_are_listed_live_through_the_gateway(db: PgPool) {
	let s = Setup::new(db).await;
	let files = s.mcp("Files").await;
	*s.agent.tools_line.lock().unwrap() = json!({
		"server": { "name": "files", "version": "1.0" },
		"tools": [{ "name": "read_file", "description": "<b>Reads</b>",
			"inputSchema": { "properties": { "path": {} } } }],
	})
	.to_string();

	// Not running yet.
	let (status, body) = s
		.api("POST", &format!("/mcp-servers/{files}/tools"), None)
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	assert!(s.agent.tools_requests.lock().unwrap().is_empty());

	s.deploy(files).await;
	let (status, tools) = s
		.api("POST", &format!("/mcp-servers/{files}/tools"), None)
		.await;
	assert_eq!(status, StatusCode::OK, "{tools}");
	assert_eq!(tools["server"], "files 1.0");
	assert_eq!(tools["tools"][0]["name"], "read_file");
	assert_eq!(tools["tools"][0]["params"], json!(["path"]));

	// The probe used the server's own key, which was pushed first and is never shown.
	let request = s.agent.tools_requests.lock().unwrap()[0].clone();
	assert_eq!(request["name"], "files");
	let key = request["key"].as_str().unwrap();
	let config = s.last_config();
	assert!(
		config
			.mcp_keys
			.iter()
			.any(|k| k.key_hash == crate::links::hash_key(key) && k.mcp.name == "files")
	);
	assert!(!tools.to_string().contains(key));

	// A second listing reuses the key; an answer that is an error is a conflict.
	let (status, _) = s
		.api("POST", &format!("/mcp-servers/{files}/tools"), None)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		s.agent.tools_requests.lock().unwrap()[1]["key"],
		request["key"]
	);
	*s.agent.tools_line.lock().unwrap() = json!({ "error": "HTTP 502: down" }).to_string();
	let (status, body) = s
		.api("POST", &format!("/mcp-servers/{files}/tools"), None)
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

impl Setup {
	async fn template(&self, id: &str, inputs: Value) -> (StatusCode, Value) {
		self.api(
			"POST",
			&format!("/templates/{id}/deploy"),
			Some(json!({ "server_id": self.server, "inputs": inputs })),
		)
		.await
	}

	/// Runs the queued deploy job of deployment `id` and returns the agent's spec.
	async fn run_deployment(&self, id: i64) -> Value {
		let job = jobs::latest_for(&self.db, JobKind::Deploy, id)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		crate::deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log)
			.await
			.unwrap();
		log.finish().await;
		self.agent.deploys.lock().unwrap().last().unwrap().clone()
	}
}

#[test]
fn mcp_resource_templates_are_applications_with_an_mcp_section() {
	for id in ["mcp-fetch", "mcp-filesystem", "mcp-memory", "mcp-github"] {
		let loaded = crate::templates::find(id).unwrap_or_else(|| panic!("{id} did not load"));
		assert!(loaded.plain_app, "{id} is deployed as one application");
		assert!(loaded.template.mcp.is_some(), "{id}");
		assert_eq!(loaded.template.category, "ai");
		assert!(loaded.template.tags.iter().any(|t| t == "mcp"));
		assert!(
			loaded.template.source.is_some() && loaded.template.docs.is_some(),
			"{id} says where it comes from"
		);
	}
}

#[sqlx::test]
async fn mcp_resource_a_template_deploys_as_a_registered_unrouted_server(db: PgPool) {
	let s = Setup::new(db).await;
	let deploy = |id: &'static str, inputs: Value| s.template(id, inputs);

	let (status, deployed) = deploy("mcp-filesystem", json!({})).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{deployed}");
	assert_eq!(deployed["kind"], "application");
	assert_eq!(deployed["urls"], json!([]));
	let id = deployed["id"].as_i64().unwrap();
	let spec = s
		.run_deployment(deployed["deploy_id"].as_i64().unwrap())
		.await;
	assert!(spec["routing"].is_null());
	assert_eq!(
		spec["options"]["mcp_bridge"],
		json!({ "port": 8080, "path": "/mcp", "command": ["node", "/app/dist/index.js", "/data"] })
	);
	assert_eq!(spec["volumes"][0]["path"], "/data");

	let (status, shown) = s.api("GET", &format!("/mcp-servers/{id}"), None).await;
	assert_eq!(status, StatusCode::OK, "{shown}");
	assert_eq!(shown["transport"], "stdio");
	assert_eq!(shown["slug"], "mcp-filesystem");

	// The image's own command is used when the template names none; the token is an answer.
	let (status, body) = deploy("mcp-github", json!({})).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	let (status, github) = deploy("mcp-github", json!({ "github_token": "ghp_secret" })).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{github}");
	let spec = s
		.run_deployment(github["deploy_id"].as_i64().unwrap())
		.await;
	assert_eq!(
		spec["options"]["mcp_bridge"],
		json!({ "port": 8080, "path": "/mcp" })
	);
	assert_eq!(
		value(&env_of(&spec), "GITHUB_PERSONAL_ACCESS_TOKEN"),
		Some("ghp_secret")
	);
	let (_, page) = s
		.api("GET", &format!("/mcp-servers/{}", github["id"]), None)
		.await;
	assert!(!page.to_string().contains("ghp_secret"));
}

mod pages {
	use super::*;
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login,
	};

	impl Setup {
		async fn cookie(&self) -> String {
			ui_login(&self.app, "alice").await
		}

		fn page(&self, mcp: i64) -> String {
			format!("/teams/{}/projects/{}/mcp/{mcp}", self.team, self.project)
		}

		async fn link_count(&self) -> i64 {
			sqlx::query_scalar("SELECT count(*) FROM application_mcp_links")
				.fetch_one(&self.db)
				.await
				.unwrap()
		}

		fn tab(&self) -> String {
			format!(
				"/teams/{}/projects/{}/apps/{}/mcp",
				self.team, self.project, self.consumer
			)
		}
	}

	#[sqlx::test]
	async fn mcp_resource_page_shows_the_connection_and_escapes_tool_text(db: PgPool) {
		let s = Setup::new(db).await;
		let cookie = s.cookie().await;
		let files = s.mcp("Files").await;
		*s.agent.tools_line.lock().unwrap() = json!({
			"tools": [{ "name": "read_file", "description": "<script>alert(1)</script>" }],
		})
		.to_string();
		s.deploy(files).await;

		let (status, _, body) = get_html(&s.app, &s.page(files), Some(&cookie)).await;
		assert_eq!(status, StatusCode::OK, "{body}");
		assert_contains(&body, "http://llm.podkit.internal:8088/mcp/files");
		assert_contains(&body, "List tools");
		assert_eq!(body.matches("<h1").count(), 1);

		let (status, _, patch) = post_signals(
			&s.app,
			&format!("{}/tools", s.page(files)),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{patch}");
		assert_contains(&patch, "read_file");
		assert_contains(&patch, "&lt;script&gt;");
		assert_not_contains(&patch, "<script>alert");
	}

	#[sqlx::test]
	async fn mcp_resource_app_tab_links_and_unlinks_without_showing_a_key(db: PgPool) {
		let s = Setup::new(db).await;
		let cookie = s.cookie().await;
		let files = s.mcp("Files").await;

		let (status, _, body) = get_html(&s.app, &s.tab(), Some(&cookie)).await;
		assert_eq!(status, StatusCode::OK, "{body}");
		assert_contains(&body, "Link an MCP server");

		let (status, _, patch) = post_signals(
			&s.app,
			&format!("{}/link", s.tab()),
			Some(&cookie),
			&json!({ "mcpServer": files.to_string() }),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{patch}");
		assert_contains(&patch, "MCP_FILES_URL");
		let hash = s.agent.configs.lock().unwrap().last().unwrap().mcp_keys[0]
			.key_hash
			.clone();
		assert_not_contains(&patch, &hash);

		let (status, _, patch) = post_signals(
			&s.app,
			&format!("{}/unlink?server={files}", s.tab()),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{patch}");
		assert_contains(&patch, "No MCP server is linked");
	}

	#[sqlx::test]
	async fn mcp_resource_pages_refuse_other_teams_and_anonymous_callers(db: PgPool) {
		let s = Setup::new(db).await;
		let files = s.mcp("Files").await;
		user_with_team(&s.app, "mallory").await;
		let mallory = ui_login(&s.app, "mallory").await;

		let (status, _, _) = get_html(&s.app, &s.page(files), Some(&mallory)).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
		let (status, _, _) = get_html(&s.app, &s.tab(), Some(&mallory)).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
		let (status, _, _) = post_signals(
			&s.app,
			&format!("{}/link", s.tab()),
			Some(&mallory),
			&json!({ "mcpServer": files.to_string() }),
		)
		.await;
		// An action's refusal is an event on the page's stream, not a status.
		assert_eq!(status, StatusCode::OK);
		assert_eq!(s.link_count().await, 0);
		assert!(
			s.agent
				.configs
				.lock()
				.unwrap()
				.iter()
				.all(|c| c.mcp_keys.is_empty())
		);

		let (status, _, _) = get_html(&s.app, &s.page(files), None).await;
		assert!(
			status.is_redirection() || status == StatusCode::UNAUTHORIZED,
			"{status}"
		);
	}
}

/// The live test: a real agent, a real Podman, a real template MCP server.
///
/// It runs its own private `podman system service` on a store under `/tmp/pk-t_b31c4e6e`, so it
/// never touches the containers or images of a running podkit. It needs the `podkit-agent`
/// binary built next to the test binary (`cargo build -p agent`), `podman` on the PATH (or named
/// by `PODMAN`), and the network for `docker.io/mcp/fetch`, `nginx:alpine`, traefik and python.
///
/// ```text
/// cargo build -p agent && cargo test -p server --all-features -- --ignored mcp_resource_live
/// ```
mod live {
	use std::path::PathBuf;
	use std::process::{Child, Command, Output, Stdio};
	use std::time::Duration;

	use super::*;

	const ROOT: &str = "/tmp/pk-t_b31c4e6e";
	const INGRESS_PORT: u16 = 21853;
	const HTTPS_PORT: u16 = 21854;

	fn podman_bin() -> String {
		std::env::var("PODMAN").unwrap_or_else(|_| "podman".to_owned())
	}

	/// A private Podman service; the images stay in its store for the next run.
	struct Private {
		service: Child,
	}

	impl Private {
		fn start() -> Self {
			let root = PathBuf::from(ROOT);
			std::fs::create_dir_all(&root).unwrap();
			let _ = std::fs::remove_file(root.join("podman.sock"));
			let service = Command::new(podman_bin())
				.arg("--root")
				.arg(root.join("store"))
				.arg("--runroot")
				.arg(root.join("run"))
				.args(["system", "service", "--time=0"])
				.arg(format!("unix://{}", root.join("podman.sock").display()))
				.stdout(Stdio::null())
				.stderr(Stdio::null())
				.spawn()
				.expect("podman system service");
			Self { service }
		}

		fn socket() -> PathBuf {
			PathBuf::from(ROOT).join("podman.sock")
		}

		fn podman(args: &[&str]) -> Output {
			Command::new(podman_bin())
				.arg("--url")
				.arg(format!("unix://{}", Self::socket().display()))
				.args(args)
				.output()
				.expect("podman")
		}

		fn text(args: &[&str]) -> String {
			let out = Self::podman(args);
			format!(
				"{}{}",
				String::from_utf8_lossy(&out.stdout),
				String::from_utf8_lossy(&out.stderr)
			)
		}
	}

	impl Drop for Private {
		fn drop(&mut self) {
			// Everything this test started is named `podkit-*`; the store holds nothing else.
			let _ = Self::podman(&["rm", "-f", "-t", "0", "-a"]);
			let networks = Self::text(&["network", "ls", "--format", "{{.Name}}"]);
			for n in networks.lines().filter(|n| n.starts_with("podkit")) {
				let _ = Self::podman(&["network", "rm", "-f", n]);
			}
			let _ = self.service.kill();
			let _ = self.service.wait();
		}
	}

	fn agent_binary() -> PathBuf {
		let exe = std::env::current_exe().unwrap();
		let binary = exe
			.parent()
			.and_then(|deps| deps.parent())
			.unwrap()
			.join("podkit-agent");
		assert!(
			binary.exists(),
			"{} is missing; run `cargo build -p agent` first",
			binary.display()
		);
		binary
	}

	/// Runs `script` in `container`; the output on success, the failure text otherwise.
	fn exec(container: &str, script: &str) -> Result<String, String> {
		let out = Private::podman(&["exec", container, "sh", "-c", script]);
		let text = String::from_utf8_lossy(&out.stdout).into_owned();
		if out.status.success() {
			Ok(text)
		} else {
			Err(format!("{text}{}", String::from_utf8_lossy(&out.stderr)))
		}
	}

	/// An MCP call from inside the app, with the link's own variables.
	fn call_mcp(container: &str, body: &str) -> Result<String, String> {
		// https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#streamable-http:
		// a client POSTs JSON-RPC and accepts both `application/json` and `text/event-stream`.
		exec(
			container,
			&format!(
				"wget -q -O- -T 20 --header \"Authorization: Bearer $MCP_MCP_FETCH_KEY\" \
				 --header 'Content-Type: application/json' \
				 --header 'Accept: application/json, text/event-stream' \
				 --post-data '{body}' \"$MCP_MCP_FETCH_URL\""
			),
		)
	}

	async fn run_deploy(s: &Setup, deployment: i64) {
		let job = jobs::latest_for(&s.db, JobKind::Deploy, deployment)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = s.state.logs.open(job.id).await.unwrap();
		let result =
			crate::deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		if let Err(e) = result {
			let text = std::fs::read_to_string(s.state.logs.path(job.id)).unwrap_or_default();
			panic!("deploy {deployment} failed: {e:?}\n{text}");
		}
	}

	fn container_of(prefix: &str) -> String {
		let out = Private::text(&[
			"ps",
			"--filter",
			&format!("name={prefix}"),
			"--format",
			"{{.Names}}",
		]);
		let names: Vec<&str> = out.lines().filter(|n| n.starts_with(prefix)).collect();
		assert_eq!(names.len(), 1, "one container named {prefix}*: {out}");
		names[0].to_owned()
	}

	#[sqlx::test]
	#[ignore = "needs podman and the podkit-agent binary; pulls mcp/fetch, nginx:alpine, traefik and python:3.13-slim into /tmp/pk-t_b31c4e6e; binds 21853 and 21854"]
	async fn mcp_resource_live_a_linked_app_lists_tools_through_the_gateway(db: PgPool) {
		let private = Private::start();
		for _ in 0..40 {
			if Private::socket().exists() && Private::podman(&["info"]).status.success() {
				break;
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
		assert!(
			Private::podman(&["info"]).status.success(),
			"the private podman service never answered"
		);

		let mut state = state(db.clone());
		{
			let config = Arc::make_mut(&mut state.config);
			config.ingress_port = INGRESS_PORT;
			config.https_port = HTTPS_PORT;
		}
		let agent_dir = tempfile::tempdir().unwrap().keep();
		let mut agent = tokio::process::Command::new(agent_binary())
			.arg("--data-dir")
			.arg(&agent_dir)
			.arg("--podman-socket")
			.arg(Private::socket())
			.arg("--socket")
			.arg(&state.config.local_agent_socket)
			.stdout(Stdio::null())
			.stderr(Stdio::inherit())
			.kill_on_drop(true)
			.spawn()
			.expect("start podkit-agent");
		for _ in 0..50 {
			if state.config.local_agent_socket.exists() {
				break;
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}

		let s = Setup::on(db, state, Arc::new(FakeAgent::default())).await;
		let result =
			futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(scenario(&s))).await;
		let _ = agent.kill().await;
		drop(private);
		if let Err(panic) = result {
			std::panic::resume_unwind(panic);
		}
	}

	async fn scenario(s: &Setup) {
		// An agent application on the same environment, from an image with `wget`.
		let (status, app) = s
			.api(
				"POST",
				"/applications",
				Some(json!({
					"name": "chat", "slug": "chat", "server_id": s.server,
					"image": "docker.io/library/nginx:alpine", "container_port": 80,
				})),
			)
			.await;
		assert_eq!(status, StatusCode::OK, "{app}");
		let app = app["id"].as_i64().unwrap();

		// The MCP server comes from a template, behind the agent's stdio bridge.
		let (status, deployed) = s.template("mcp-fetch", json!({})).await;
		assert_eq!(status, StatusCode::ACCEPTED, "{deployed}");
		let mcp = deployed["id"].as_i64().unwrap();
		run_deploy(s, deployed["deploy_id"].as_i64().unwrap()).await;

		let (status, link) = s
			.api(
				"POST",
				&format!("/applications/{app}/mcp"),
				Some(json!({ "mcp_server_id": mcp })),
			)
			.await;
		assert_eq!(status, StatusCode::CREATED, "{link}");

		// The next deploy of the app carries the link's variables.
		let (status, deployment) = s
			.api("POST", &format!("/applications/{app}/deployments"), None)
			.await;
		assert_eq!(status, StatusCode::ACCEPTED, "{deployment}");
		run_deploy(s, deployment["id"].as_i64().unwrap()).await;
		let container = container_of(&format!("podkit-app-{app}-"));

		let url = exec(&container, "echo \"$MCP_MCP_FETCH_URL\"").unwrap();
		assert_eq!(
			url.trim(),
			"http://llm.podkit.internal:8088/mcp/mcp-fetch",
			"the app got its link URL"
		);

		// The first answer waits for the image's server to start.
		let mut initialized = Err(String::new());
		for _ in 0..60 {
			initialized = call_mcp(
				&container,
				r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"live","version":"1"}}}"#,
			);
			if initialized.is_ok() {
				break;
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
		let initialized = initialized.unwrap_or_else(|e| panic!("initialize never answered: {e}"));
		assert!(initialized.contains("protocolVersion"), "{initialized}");

		let tools = call_mcp(
			&container,
			r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
		)
		.unwrap();
		assert!(tools.contains("\"tools\""), "{tools}");
		assert!(
			tools.contains("\"fetch\""),
			"the fetch server lists its fetch tool: {tools}"
		);

		// A wrong key is refused by the gateway.
		let refused = exec(
			&container,
			"wget -q -O- -T 10 --header 'Authorization: Bearer nope' \
			 --post-data '{}' \"$MCP_MCP_FETCH_URL\" 2>&1",
		);
		assert!(refused.is_err(), "a wrong key must not be served");
	}
}
