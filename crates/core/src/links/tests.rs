//! Link tests against a fake agent on the local server's socket.

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
}

fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	let (deploy, config, drain) = (agent.clone(), agent.clone(), agent.clone());
	let app = Router::new()
		.route(
			"/v1/deploy",
			post(move |body: Bytes| async move {
				deploy
					.deploys
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				let events = [
					json!({ "type": "log", "stream": "system", "line": "deploying" }),
					json!({ "type": "deployed", "container_id": "c1", "image": "x",
						"commit_sha": null }),
				];
				let body = events.iter().fold(String::new(), |mut out, e| {
					out.push_str(&e.to_string());
					out.push('\n');
					out
				});
				([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
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
	application: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let agent = fake_agent(&state);
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (project, server) = Self::project_and_server(&app, &token, team).await;
		let (status, application) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(&token),
			Some(json!({
				"name": "chat", "slug": "chat", "server_id": server,
				"repo_url": "https://github.com/acme/chat", "git_ref": "main",
				"container_port": 3000,
			})),
		)
		.await;
		assert!(status.is_success(), "{application}");
		Self {
			app,
			state,
			db,
			agent,
			token,
			team,
			project,
			server,
			application: application["id"].as_i64().unwrap(),
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
			Some(json!({ "name": "ai", "slug": "ai" })),
		)
		.await;
		assert!(status.is_success(), "{project}");
		(
			project["id"].as_i64().unwrap(),
			servers[0]["id"].as_i64().unwrap(),
		)
	}

	/// A model row on `server`, with the files it needs.
	async fn add_model(&self, team: i64, project: i64, on_server: i64, served: &str) -> i64 {
		let (files, model) = (crypto::generate_id(), crypto::generate_id());
		sqlx::query(
			"INSERT INTO model_files
				(id, team_id, server_id, repo, revision, selection, total_bytes, downloaded_bytes,
				 status, volume_name)
			VALUES ($1, $2, $3, 'acme/open', 'main', '{\"include\":[],\"files\":[]}', 1, 1,
				'ready', $4)",
		)
		.bind(files)
		.bind(team)
		.bind(on_server)
		.bind(format!("model-files-{files}"))
		.execute(&self.db)
		.await
		.unwrap();
		sqlx::query(
			"INSERT INTO models
				(id, project_id, server_id, model_files_id, name, slug, engine, image, port,
				 model_path, served_name, context_size, status)
			VALUES ($1, $2, $3, $4, $5, $5, 'llama_cpu', 'img', 8080, 'q.gguf', $6, 4096, 'running')",
		)
		.bind(model)
		.bind(project)
		.bind(on_server)
		.bind(files)
		.bind(format!("slug-{model}"))
		.bind(served)
		.execute(&self.db)
		.await
		.unwrap();
		model
	}

	async fn mine(&self, served: &str) -> i64 {
		self.add_model(self.team, self.project, self.server, served)
			.await
	}

	fn path(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/models{rest}",
			self.team, self.project, self.application
		)
	}

	async fn call(&self, method: &str, rest: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, &self.path(rest), Some(&self.token), body).await
	}

	async fn link(&self, model: i64) -> (StatusCode, Value) {
		self.call("POST", "", Some(json!({ "model_id": model })))
			.await
	}

	/// Deploys the application and returns the env the agent was asked to run it with.
	async fn deploy_env(&self) -> Vec<(String, String)> {
		let (status, deployment) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications/{}/deployments",
				self.team, self.project, self.application
			),
			Some(&self.token),
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
		let spec = self.agent.deploys.lock().unwrap().last().unwrap().clone();
		serde_json::from_value(spec["env"].clone()).unwrap()
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

fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
	env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn links_key_hash_matches_the_agent() {
	// SHA-256 of "abc"; the agent's test checks the same vector.
	assert_eq!(
		super::hash_key("abc"),
		"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
	);
}

#[sqlx::test]
async fn links_first_link_is_the_default_and_the_agent_gets_the_table(db: PgPool) {
	let s = Setup::new(db).await;
	let (qwen, phi) = (s.mine("qwen").await, s.mine("phi").await);

	let (status, link) = s.link(qwen).await;
	assert_eq!(status, StatusCode::CREATED, "{link}");
	assert_eq!(link["served_name"], "qwen");
	assert_eq!(link["is_default"], true);
	assert!(link.get("key").is_none() && link.get("api_key").is_none());
	let (_, link) = s.link(phi).await;
	assert_eq!(link["is_default"], false);

	let config = s.last_config();
	assert_eq!(config.keys.len(), 1);
	let names: Vec<_> = config.keys[0]
		.models
		.iter()
		.map(|m| m.name.as_str())
		.collect();
	assert_eq!(names, ["qwen", "phi"]);
	let model = &config.keys[0].models[0];
	assert_eq!((model.model_id, model.port), (qwen, 8080));
	assert!(model.host.starts_with("model-slug-"));
	assert!(model.stream_usage);
	assert!(config.other_models.contains(&"phi".to_owned()));

	let (status, put) = s.call("PUT", &format!("/{phi}/default"), None).await;
	assert_eq!(status, StatusCode::OK, "{put}");
	let (_, list) = s.call("GET", "", None).await;
	let defaults: Vec<_> = list
		.as_array()
		.unwrap()
		.iter()
		.map(|l| l["is_default"].as_bool().unwrap())
		.collect();
	assert_eq!(defaults, [false, true]);
}

#[sqlx::test]
async fn links_deploy_env_has_the_five_variables_until_unlinked(db: PgPool) {
	let s = Setup::new(db).await;
	let (qwen, phi) = (s.mine("qwen").await, s.mine("phi").await);

	assert!(value(&s.deploy_env().await, "OPENAI_API_KEY").is_none());

	// A user's variable of the same name must not win over the gateway's.
	let (status, _) = call(
		&s.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/env-vars",
			s.team, s.project, s.application
		),
		Some(&s.token),
		Some(json!({ "key": "OPENAI_BASE_URL", "value": "https://api.openai.com/v1" })),
	)
	.await;
	assert!(status.is_success());

	s.link(qwen).await;
	s.link(phi).await;
	let env = s.deploy_env().await;
	assert_eq!(
		value(&env, "OPENAI_BASE_URL"),
		Some("http://llm.podkit.internal:8088/v1")
	);
	assert_eq!(
		env.iter().filter(|(k, _)| k == "OPENAI_BASE_URL").count(),
		1
	);
	assert_eq!(
		value(&env, "ANTHROPIC_BASE_URL"),
		Some("http://llm.podkit.internal:8088")
	);
	assert_eq!(value(&env, "PODKIT_MODEL"), Some("qwen"));
	let key = value(&env, "OPENAI_API_KEY").unwrap();
	assert!(key.starts_with("pk-") && key.len() > 40);
	assert_eq!(value(&env, "ANTHROPIC_API_KEY"), Some(key));
	// The agent knows the key by its hash only.
	let config = s.last_config();
	assert_eq!(config.keys[0].key_hash, super::hash_key(key));
	let stored: Vec<u8> = sqlx::query_scalar("SELECT key FROM gateway_keys")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&stored).contains(key));
	// The key survives redeploys.
	assert_eq!(value(&s.deploy_env().await, "OPENAI_API_KEY"), Some(key));

	// Unlinking the default hands the default to the other one.
	let (status, _) = s.call("DELETE", &format!("/{qwen}"), None).await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	let env = s.deploy_env().await;
	assert_eq!(value(&env, "PODKIT_MODEL"), Some("phi"));
	assert_eq!(value(&env, "OPENAI_API_KEY"), Some(key));
	assert_eq!(s.last_config().keys[0].models.len(), 1);

	// Unlinking the last one takes the variables and the key away.
	s.call("DELETE", &format!("/{phi}"), None).await;
	let env = s.deploy_env().await;
	for name in [
		"OPENAI_API_KEY",
		"ANTHROPIC_BASE_URL",
		"ANTHROPIC_API_KEY",
		"PODKIT_MODEL",
	] {
		assert!(value(&env, name).is_none(), "{name} is still set");
	}
	// What is left of OPENAI_BASE_URL is the user's own.
	assert_eq!(
		value(&env, "OPENAI_BASE_URL"),
		Some("https://api.openai.com/v1")
	);
	assert!(s.last_config().keys.is_empty());
	let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gateway_keys")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(keys, 0);
}

#[sqlx::test]
async fn links_refuse_what_cannot_work(db: PgPool) {
	let s = Setup::new(db.clone()).await;
	let qwen = s.mine("qwen").await;
	s.link(qwen).await;

	// Another model of the same served name.
	let twin = s.mine("qwen").await;
	let (status, body) = s.link(twin).await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");

	// A model on another server.
	let other_server = crypto::generate_id();
	let other_key = crate::tests::seed_key(&db, s.team, "remote key").await;
	sqlx::query(
		"INSERT INTO servers (id, team_id, name, hostname, ssh_port, ssh_user, ssh_key_id,
			podman_socket_path, is_local, status)
		VALUES ($1, $2, 'remote', 'remote.example', 22, 'root', $3,
			'/run/podman.sock', FALSE, 'active')",
	)
	.bind(other_server)
	.bind(s.team)
	.bind(other_key)
	.execute(&db)
	.await
	.unwrap();
	let remote = s.add_model(s.team, s.project, other_server, "remote").await;
	let (status, body) = s.link(remote).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

	// A model of another team is a miss, and so is the application.
	let (bob_token, bob_team) = user_with_team(&s.app, "bob").await;
	let (bob_project, bob_server) = Setup::project_and_server(&s.app, &bob_token, bob_team).await;
	let theirs = s
		.add_model(bob_team, bob_project, bob_server, "theirs")
		.await;
	let (status, _) = s.link(theirs).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = call(
		&s.app,
		"POST",
		&s.path(""),
		Some(&bob_token),
		Some(json!({ "model_id": qwen })),
	)
	.await;
	assert_ne!(status, StatusCode::CREATED);
	let (status, _) = call(&s.app, "GET", &s.path(""), Some(&bob_token), None).await;
	assert!(status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN);

	// Not linked.
	let (status, _) = s.call("DELETE", &format!("/{twin}"), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = s.call("PUT", &format!("/{twin}/default"), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// A linked model cannot be removed from under the application.
	let (status, body) = call(
		&s.app,
		"DELETE",
		&format!(
			"/api/v1/teams/{}/projects/{}/models/{qwen}",
			s.team, s.project
		),
		Some(&s.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn links_usage_is_added_up_per_key_model_and_hour(db: PgPool) {
	let s = Setup::new(db).await;
	let qwen = s.mine("qwen").await;
	s.link(qwen).await;
	let hash = s.last_config().keys[0].key_hash.clone();
	let server = crate::servers::find(&s.db, crate::ids::ServerId(s.server))
		.await
		.unwrap()
		.unwrap();
	let client = s.state.agents.client(&s.state, &server).await.unwrap();

	for requests in [2, 3] {
		*s.agent.usage.lock().unwrap() = vec![GatewayUsage {
			key_hash: hash.clone(),
			model_id: qwen,
			hour: 1_700_000_000 - 1_700_000_000 % 3600,
			requests,
			input_tokens: 10 * requests,
			output_tokens: 4 * requests,
			mcp: false,
		}];
		super::collect_usage(&s.state, &client).await.unwrap();
	}
	let (rows, requests, input, output, application, team): (i64, i64, i64, i64, i64, i64) =
		sqlx::query_as(
			"SELECT COUNT(*), SUM(requests)::bigint, SUM(input_tokens)::bigint,
				SUM(output_tokens)::bigint, MIN(application_id), MIN(team_id) FROM model_usage",
		)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!((rows, requests, input, output), (1, 5, 50, 20));
	assert_eq!((application, team), (s.application, s.team));

	// Nothing left to take.
	assert_eq!(super::collect_usage(&s.state, &client).await.unwrap(), 0);
}
