//! MCP server resource tests against a fake agent on the local server's socket.

use std::sync::Arc;

use axum::http::StatusCode;
use proto::GatewayUsage;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::jobs::{self, JobKind};
use crate::tests::mcp_servers::{FakeAgent, Setup};
use crate::tests::{call, state, user_with_team};

fn env_of(spec: &Value) -> Vec<(String, String)> {
	serde_json::from_value(spec["env"].clone()).unwrap()
}

fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
	env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

const TOKEN: &str = "PKTOKEN-in-an-argument-1234";

const TOKEN2: &str = "PKTOKEN-second-5678";

async fn stored_command(db: &PgPool, id: i64) -> (Option<String>, Option<Vec<u8>>) {
	sqlx::query_as("SELECT command, command_enc FROM mcp_servers WHERE application_id = $1")
		.bind(id)
		.fetch_one(db)
		.await
		.unwrap()
}

/// SEC-05: the command line of a stdio server may carry a token. It is stored encrypted, the API
/// shows `•••` unless the caller reveals it with `read:sensitive` and `update application`, and
/// the audit log never holds it.
#[sqlx::test]
async fn sec_cleanup_an_mcp_stdio_command_is_sealed_masked_and_redacted_in_the_audit(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (status, made) = s
		.api(
			"POST",
			"/mcp-servers",
			Some(json!({
				"name": "tokened", "server_id": s.server, "image": "ghcr.io/acme/mcp:1",
				"transport": "stdio",
				"command": ["node", "server.js", format!("--api-key={TOKEN}")],
			})),
		)
		.await;
	assert_eq!(status, StatusCode::CREATED, "{made}");
	assert_eq!(made["command"], json!(["•••", "•••", "•••"]), "{made}");
	let id = made["id"].as_i64().unwrap();

	// At rest: no plain text, and the ciphertext does not hold the token.
	let (plain, sealed) = stored_command(&db, id).await;
	assert_eq!(plain, None, "the plain column stays empty");
	let sealed = sealed.expect("the command is stored encrypted");
	assert!(!sealed.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));

	// In answers: masked, in the list's detail and after an update.
	let (_, shown) = s.api("GET", &format!("/mcp-servers/{id}"), None).await;
	assert_eq!(shown["command"], json!(["•••", "•••", "•••"]), "{shown}");
	let (status, updated) = s
		.api(
			"PUT",
			&format!("/mcp-servers/{id}"),
			Some(json!({ "command": ["node", format!("--token={TOKEN2}")] })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{updated}");
	assert_eq!(updated["command"], json!(["•••", "•••"]), "{updated}");
	assert!(!updated.to_string().contains(TOKEN2));

	// In the audit log: that it changed and how long it is, never what it says.
	let events: Vec<(String, Value)> = sqlx::query_as(
		"SELECT action, diff FROM audit_events WHERE action LIKE 'mcp.%' ORDER BY id",
	)
	.fetch_all(&db)
	.await
	.unwrap();
	let update = events
		.iter()
		.find(|(a, _)| a == "mcp.update")
		.expect("the update is audited");
	assert_eq!(update.1["command"], json!("•••"), "{}", update.1);
	assert_eq!(update.1["arguments"], json!(2), "{}", update.1);
	for (action, diff) in &events {
		let text = diff.to_string();
		assert!(
			!text.contains(TOKEN) && !text.contains(TOKEN2),
			"{action} leaks the command: {text}"
		);
	}
	let everything: String = sqlx::query_scalar(
		"SELECT COALESCE(string_agg(summary || diff::text, ' '), '') FROM audit_events",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert!(!everything.contains("PKTOKEN"), "{everything}");

	// Revealing needs `read:sensitive` and says so in the log; a plain `read` token is refused.
	let (status, revealed) = s
		.api("GET", &format!("/mcp-servers/{id}?reveal=true"), None)
		.await;
	assert_eq!(status, StatusCode::OK, "{revealed}");
	assert_eq!(
		revealed["command"],
		json!(["node", format!("--token={TOKEN2}")])
	);
	let reveal: Value = sqlx::query_scalar(
		"SELECT diff FROM audit_events WHERE action = 'mcp.reveal' ORDER BY id DESC LIMIT 1",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(reveal, json!({ "command": "•••" }));
	let (status, read_token) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{}/api-tokens", s.team),
		Some(&s.token),
		Some(json!({ "name": "reader", "scopes": ["read"] })),
	)
	.await;
	assert!(status.is_success(), "{read_token}");
	let reader = read_token["secret"].as_str().unwrap();
	let (status, _) = call(
		&s.app,
		"GET",
		&format!("{}/mcp-servers/{id}?reveal=true", s.base()),
		Some(reader),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	let (status, masked) = call(
		&s.app,
		"GET",
		&format!("{}/mcp-servers/{id}", s.base()),
		Some(reader),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(masked["command"], json!(["•••", "•••"]));

	// The agent still gets the real command.
	let spec = s.deploy(id).await;
	assert_eq!(
		spec["options"]["mcp_bridge"]["command"],
		json!(["node", format!("--token={TOKEN2}")])
	);
}

/// Rows written before migration 0074 hold the command in plain text; start-up seals them.
#[sqlx::test]
async fn sec_cleanup_a_command_stored_in_plain_text_is_sealed_at_start_up(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let (_, made) = s.server("Legacy", "stdio").await;
	let id = made["id"].as_i64().unwrap();
	sqlx::query(
		"UPDATE mcp_servers SET command = 'node' || E'\\n' || '--token=OLD', command_enc = NULL
		WHERE application_id = $1",
	)
	.bind(id)
	.execute(&db)
	.await
	.unwrap();
	assert_eq!(
		crate::mcp_servers::seal_legacy(&db, &s.state.secrets)
			.await
			.unwrap(),
		1
	);
	let (plain, sealed) = stored_command(&db, id).await;
	assert_eq!((plain, sealed.is_some()), (None, true));
	let (_, revealed) = s
		.api("GET", &format!("/mcp-servers/{id}?reveal=true"), None)
		.await;
	assert_eq!(revealed["command"], json!(["node", "--token=OLD"]));
	// A second pass has nothing to do.
	assert_eq!(
		crate::mcp_servers::seal_legacy(&db, &s.state.secrets)
			.await
			.unwrap(),
		0
	);
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

/// The live test: a real agent, a real Podman, a real template MCP server.
///
/// It runs its own private `podman system service` on a store under `/tmp/pk-t_b31c4e6e`, so it
/// never touches the containers or images of a running podkit. It needs the `podkit-agent`
/// binary built next to the test binary (`cargo build -p agent`), `podman` on the PATH (or named
/// by `PODMAN`), and the network for `docker.io/mcp/fetch`, `nginx:alpine`, traefik and python.
///
/// ```text
/// cargo build -p agent && cargo test -p podkit-core --all-features -- --ignored mcp_resource_live
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
		let mut agent = tokio::process::Command::new(crate::tests::agent_binary())
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
