//! The fixtures the MCP servers tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `mcp_servers::tests`.

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
pub struct FakeAgent {
	pub deploys: Mutex<Vec<Value>>,
	pub configs: Mutex<Vec<GatewayConfig>>,
	/// What the next drain returns.
	pub usage: Mutex<Vec<GatewayUsage>>,
	/// What the tools probe prints.
	pub tools_line: Mutex<String>,
	pub tools_requests: Mutex<Vec<Value>>,
}

pub fn ndjson(events: &[Value]) -> axum::response::Response {
	let body = events.iter().fold(String::new(), |mut out, e| {
		out.push_str(&e.to_string());
		out.push('\n');
		out
	});
	([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

pub fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
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

pub struct Setup {
	pub app: Router,
	pub state: AppState,
	pub db: PgPool,
	pub agent: Arc<FakeAgent>,
	pub token: String,
	pub team: i64,
	pub project: i64,
	pub server: i64,
	/// An ordinary application that links to MCP servers.
	pub consumer: i64,
}

impl Setup {
	pub async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let agent = fake_agent(&state);
		Self::on(db, state, agent).await
	}

	/// The same project on `state`, whatever answers on its agent socket.
	pub async fn on(db: PgPool, state: AppState, agent: Arc<FakeAgent>) -> Self {
		let app = crate::routes::routes(state.clone()).await.unwrap();
		Self::over(db, state, agent, app).await
	}

	/// [`Setup::on`] over a router somebody else built on `state`: `podkit-ui` passes the
	/// one with the pages.
	pub async fn over(db: PgPool, state: AppState, agent: Arc<FakeAgent>, app: Router) -> Self {
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

	pub async fn project_and_server(app: &Router, token: &str, team: i64) -> (i64, i64) {
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

	pub fn base(&self) -> String {
		format!("/api/v1/teams/{}/projects/{}", self.team, self.project)
	}

	pub async fn api(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
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
	pub async fn server_on(&self, server: i64, name: &str, transport: &str) -> (StatusCode, Value) {
		let mut body = json!({
			"name": name, "server_id": server,
			"image": "ghcr.io/acme/mcp-files:1", "transport": transport,
		});
		if transport == "stdio" {
			body["command"] = json!(["node", "dist/index.js", "/data"]);
		}
		self.api("POST", "/mcp-servers", Some(body)).await
	}

	pub async fn server(&self, name: &str, transport: &str) -> (StatusCode, Value) {
		self.server_on(self.server, name, transport).await
	}

	pub async fn mcp(&self, name: &str) -> i64 {
		let (status, body) = self.server(name, "http").await;
		assert_eq!(status, StatusCode::CREATED, "{body}");
		body["id"].as_i64().unwrap()
	}

	pub async fn link(&self, mcp: i64) -> (StatusCode, Value) {
		self.api(
			"POST",
			&format!("/applications/{}/mcp", self.consumer),
			Some(json!({ "mcp_server_id": mcp })),
		)
		.await
	}

	/// Deploys an application and returns the spec the agent was asked to run.
	pub async fn deploy(&self, application: i64) -> Value {
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

	pub fn last_config(&self) -> GatewayConfig {
		self.agent
			.configs
			.lock()
			.unwrap()
			.last()
			.expect("the table was pushed")
			.clone()
	}
}

impl Setup {
	pub async fn template(&self, id: &str, inputs: Value) -> (StatusCode, Value) {
		self.api(
			"POST",
			&format!("/templates/{id}/deploy"),
			Some(json!({ "server_id": self.server, "inputs": inputs })),
		)
		.await
	}

	/// Runs the queued deploy job of deployment `id` and returns the agent's spec.
	pub async fn run_deployment(&self, id: i64) -> Value {
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
