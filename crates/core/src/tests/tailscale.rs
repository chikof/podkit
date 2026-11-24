//! The fixtures the Tailscale tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `tailscale::tests`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, post};
use proto::{Ingress, NetworkAttach};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::ids::ApplicationId;
use crate::jobs::{self, JobFailure, JobKind};
use crate::state::AppState;
use crate::tailscale::run;
use crate::tailscale::spec::Sidecar;
use crate::tests::{call, state, user_with_team};

pub const KEY: &str = "tskey-auth-kABC123CNTRL-0123456789abcdefghijklmnopqrstuvwx";

// --- the sidecar's deploy request ----------------------------------------------------------------

pub fn sidecar<'a>(
	login_server: Option<&'a str>,
	tags: &'a [String],
	hostname: &'a str,
) -> Sidecar<'a> {
	Sidecar {
		application: ApplicationId(7),
		hostname,
		container_port: 3000,
		auth_key: KEY,
		login_server,
		tags,
		network: NetworkAttach {
			name: "pk-env-9".to_owned(),
			aliases: Vec::new(),
			internal: false,
		},
		ingress: Ingress {
			http_port: 80,
			https_port: 443,
			acme_email: None,
		},
		instance_id: 41,
	}
}

// --- a fake agent ------------------------------------------------------------------------------

#[derive(Default)]
pub struct FakeAgent {
	pub deploys: Mutex<Vec<Value>>,
	/// Every call that changes the sidecar, in order: `exec <container> <args…>`,
	/// `delete service <name>`, `delete volume <name>`.
	pub calls: Mutex<Vec<String>>,
	/// What `tailscale status --json` prints.
	pub status: Mutex<String>,
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
	let agent = Arc::new(FakeAgent {
		status: Mutex::new(
			r#"{"BackendState":"Running","Self":{"DNSName":"shop.tail1234.ts.net."}}"#.to_owned(),
		),
		..FakeAgent::default()
	});
	let (deploy, exec, service, volume) =
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
					json!({ "type": "log", "stream": "system", "line": "pulling" }),
					json!({ "type": "deployed", "container_id": "side1", "image": "x",
						"commit_sha": null }),
				])
			}),
		)
		.route(
			"/v1/containers/{id}/exec",
			post(move |Path(id): Path<String>, body: Bytes| async move {
				let req: Value = serde_json::from_slice(&body).unwrap();
				let cmd: Vec<String> = serde_json::from_value(req["cmd"].clone()).unwrap();
				exec.calls
					.lock()
					.unwrap()
					.push(format!("exec {id} {}", cmd.join(" ")));
				let output = if cmd.contains(&"status".to_owned()) {
					exec.status.lock().unwrap().clone()
				} else {
					String::new()
				};
				axum::Json(json!({ "exit_code": 0, "output": output }))
			}),
		)
		.route(
			"/v1/services/{name}",
			delete(move |Path(name): Path<String>| async move {
				service
					.calls
					.lock()
					.unwrap()
					.push(format!("delete service {name}"));
				StatusCode::NO_CONTENT
			}),
		)
		.route(
			"/v1/volumes/{name}",
			delete(move |Path(name): Path<String>| async move {
				volume
					.calls
					.lock()
					.unwrap()
					.push(format!("delete volume {name}"));
				StatusCode::NO_CONTENT
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
	pub application: i64,
}

impl Setup {
	pub async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		Self::over(db, state, app).await
	}

	/// [`Setup::new`] over a router somebody else built on `state`: `podkit-ui` passes the
	/// one with the pages.
	pub async fn over(db: PgPool, state: AppState, app: Router) -> Self {
		let agent = fake_agent(&state);
		let (token, team) = user_with_team(&app, "alice").await;
		let (project, application) = Self::project_and_app(&app, &token, team, "shop").await;
		Self {
			app,
			state,
			db,
			agent,
			token,
			team,
			project,
			application,
		}
	}

	pub async fn project_and_app(app: &Router, token: &str, team: i64, slug: &str) -> (i64, i64) {
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
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(status.is_success(), "{project}");
		let project = project["id"].as_i64().unwrap();
		let (status, application) = call(
			app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(token),
			Some(json!({
				"name": slug, "slug": slug, "server_id": servers[0]["id"],
				"repo_url": "https://github.com/acme/shop", "git_ref": "main",
				"container_port": 3000,
			})),
		)
		.await;
		assert!(status.is_success(), "{application}");
		(project, application["id"].as_i64().unwrap())
	}

	pub fn settings_path(&self) -> String {
		format!("/api/v1/teams/{}/tailscale", self.team)
	}

	pub fn exposure_path(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/tailscale",
			self.team, self.project, self.application
		)
	}

	pub async fn put_settings(&self, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"PUT",
			&self.settings_path(),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	pub async fn store_key(&self) {
		let (status, body) = self.put_settings(json!({ "auth_key": KEY })).await;
		assert_eq!(status, StatusCode::OK, "{body}");
	}

	pub async fn enable(&self, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"PUT",
			&self.exposure_path(),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	/// Runs the queued job of `kind` for the application, as the worker would.
	pub async fn run_job(&self, kind: JobKind) -> Result<(), JobFailure> {
		let job = jobs::latest_for(&self.db, kind, self.application)
			.await
			.unwrap()
			.expect("the job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let result = run::run(&self.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		result
	}

	pub async fn status(&self) -> Value {
		let (status, body) = call(
			&self.app,
			"GET",
			&self.exposure_path(),
			Some(&self.token),
			None,
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{body}");
		body["exposure"].clone()
	}
}
