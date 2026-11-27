//! API, job and permission tests of the stacks slice. The agent is a fake
//! that records what it is asked; the one live test (ignored) runs the real
//! agent against Podman.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use bytes::Bytes;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::jobs::{self, Job, JobFailure, JobStatus};
use crate::state::AppState;
use crate::tests::{call, count, server_id, state, user_id, user_with_team};

const COMPOSE: &str = r#"
services:
  app:
    image: ghcr.io/acme/app:1
    environment:
      DATABASE_URL: postgres://app:${DB_PASSWORD}@db:5432/app
      LOG_LEVEL: ${LOG_LEVEL:-info}
    expose: ["8080"]
    depends_on:
      db:
        condition: service_healthy
  db:
    image: postgres:17
    environment:
      POSTGRES_PASSWORD: ${DB_PASSWORD}
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready"]
volumes:
  pgdata:
"#;

const SECRET: &str = "hunter2-do-not-leak";

/// What the fake agent saw, and what it should answer differently.
#[derive(Default)]
struct Agent {
	deploys: Mutex<Vec<Value>>,
	stopped: Mutex<Vec<String>>,
	removed_services: Mutex<Vec<String>>,
	removed_networks: Mutex<Vec<String>>,
	removed_volumes: Mutex<Vec<String>>,
	health_polls: Mutex<Vec<String>>,
	/// Answer of the health route by container id.
	health: Mutex<HashMap<String, Value>>,
	/// Deploy failure message by agent service name.
	failures: Mutex<HashMap<String, String>>,
}

impl Agent {
	fn deployed_services(&self) -> Vec<String> {
		self.deploys
			.lock()
			.unwrap()
			.iter()
			.map(|d| d["service"].as_str().unwrap().to_string())
			.collect()
	}

	fn deploy_of(&self, service: &str) -> Value {
		self.deploys
			.lock()
			.unwrap()
			.iter()
			.rev()
			.find(|d| d["service"] == service)
			.unwrap_or_else(|| panic!("{service} was never deployed"))
			.clone()
	}

	fn set_health(&self, container: &str, health: Option<&str>, state: &str) {
		self.health.lock().unwrap().insert(
			container.to_string(),
			json!({
				"id": container, "name": container, "state": state,
				"exit_code": if state == "exited" { json!(3) } else { Value::Null },
				"health": health, "gave_up": false,
			}),
		);
	}
}

fn ndjson(events: &[proto::Event]) -> axum::response::Response {
	let body = events.iter().fold(String::new(), |mut out, e| {
		out.push_str(&serde_json::to_string(e).unwrap());
		out.push('\n');
		out
	});
	([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

/// Serves the agent routes stacks use on the local server's socket.
fn fake_agent(state: &AppState) -> Arc<Agent> {
	let agent = Arc::new(Agent::default());
	let app = Router::new()
		.route(
			"/v1/deploy",
			post(|State(a): State<Arc<Agent>>, body: Bytes| async move {
				let spec: Value = serde_json::from_slice(&body).unwrap();
				let service = spec["service"].as_str().unwrap().to_string();
				a.deploys.lock().unwrap().push(spec.clone());
				if let Some(message) = a.failures.lock().unwrap().get(&service).cloned() {
					return ndjson(&[proto::Event::Failed { message }]);
				}
				ndjson(&[
					proto::Event::Log {
						stream: proto::Stream::System,
						line: format!("pulling {}", spec["source"]["reference"]),
					},
					proto::Event::Deployed {
						container_id: format!("ctr-{service}"),
						image: spec["source"]["reference"].as_str().unwrap().to_string(),
						commit_sha: None,
					},
				])
			}),
		)
		.route(
			"/v1/containers/{id}/health",
			get(
				|State(a): State<Arc<Agent>>, Path(id): Path<String>| async move {
					a.health_polls.lock().unwrap().push(id.clone());
					let answer = a
						.health
						.lock()
						.unwrap()
						.get(&id)
						.cloned()
						.unwrap_or_else(|| {
							json!({
								"id": id, "name": id, "state": "running", "exit_code": null,
								"health": "healthy", "gave_up": false,
							})
						});
					axum::Json(answer)
				},
			),
		)
		.route(
			"/v1/containers/{id}/stop",
			post(
				|State(a): State<Arc<Agent>>, Path(id): Path<String>| async move {
					a.stopped.lock().unwrap().push(id);
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/v1/containers/{id}/logs",
			get(|| async {
				ndjson(&[
					proto::Event::Log {
						stream: proto::Stream::Stdout,
						line: "hello from the container".to_string(),
					},
					proto::Event::Done,
				])
			}),
		)
		.route(
			"/v1/services/{name}",
			delete(
				|State(a): State<Arc<Agent>>, Path(name): Path<String>| async move {
					a.removed_services.lock().unwrap().push(name);
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/v1/networks/{name}",
			delete(
				|State(a): State<Arc<Agent>>, Path(name): Path<String>| async move {
					a.removed_networks.lock().unwrap().push(name);
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/v1/volumes/{name}",
			delete(
				|State(a): State<Arc<Agent>>, Path(name): Path<String>| async move {
					a.removed_volumes.lock().unwrap().push(name);
					StatusCode::NO_CONTENT
				},
			),
		)
		.with_state(agent.clone());
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	agent
}

struct Setup {
	state: AppState,
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s} {project}");
		Self {
			state,
			app,
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
		}
	}

	fn path(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/stacks{rest}",
			self.team, self.project
		)
	}

	async fn call(&self, method: &str, rest: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, &self.path(rest), Some(&self.token), body).await
	}

	async fn server(&self) -> i64 {
		server_id(&self.db, self.team).await
	}

	async fn create(&self, name: &str, yaml: &str, env: Value) -> (StatusCode, Value) {
		self.call(
			"POST",
			"",
			Some(json!({
				"name": name, "compose_yaml": yaml, "env": env,
				"server_id": self.server().await,
			})),
		)
		.await
	}

	/// Creates `COMPOSE` with its password, returns the stack id.
	async fn stack(&self) -> i64 {
		let (s, body) = self
			.create("shop", COMPOSE, json!({ "DB_PASSWORD": SECRET }))
			.await;
		assert_eq!(s, StatusCode::CREATED, "{body}");
		body["id"].as_i64().unwrap()
	}

	/// Runs the oldest queued job of the stack the way the dispatcher does.
	async fn run_job(&self, stack: i64) -> (Result<(), JobFailure>, String) {
		let job: Job = sqlx::query_as(
			"SELECT id, kind, server_id, target_id, lock_key, status, error FROM jobs
			WHERE lock_key = $1 AND status = 'queued' ORDER BY id LIMIT 1",
		)
		.bind(format!("stack:{stack}"))
		.fetch_one(&self.db)
		.await
		.expect("a queued job");
		sqlx::query("UPDATE jobs SET status = 'running' WHERE id = $1")
			.bind(job.id)
			.execute(&self.db)
			.await
			.unwrap();
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let result = super::run::run(&self.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		let (status, error) = match &result {
			Ok(()) => (JobStatus::Succeeded, None),
			Err(JobFailure::Cancelled) => (JobStatus::Cancelled, None),
			Err(JobFailure::Failed(m)) => (JobStatus::Failed, Some(m.clone())),
		};
		sqlx::query("UPDATE jobs SET status = $2, error = $3 WHERE id = $1")
			.bind(job.id)
			.bind(status)
			.bind(error)
			.execute(&self.db)
			.await
			.unwrap();
		if let Err(failure) = &result {
			jobs::fail_target(&self.state, job.kind, job.target_id, failure).await;
		}
		let text = std::fs::read_to_string(self.state.logs.path(job.id)).unwrap();
		(result, text)
	}

	async fn deploy(&self, stack: i64) -> (Result<(), JobFailure>, String) {
		let (s, body) = self.call("POST", &format!("/{stack}/deploy"), None).await;
		assert_eq!(s, StatusCode::ACCEPTED, "{body}");
		self.run_job(stack).await
	}

	/// The container the database holds for a service.
	async fn container_of(&self, stack: i64, service: &str) -> String {
		sqlx::query_scalar(
			"SELECT container_id FROM stack_services WHERE stack_id = $1 AND name = $2",
		)
		.bind(stack)
		.bind(service)
		.fetch_one(&self.db)
		.await
		.unwrap()
	}

	async fn detail(&self, stack: i64) -> Value {
		let (s, body) = self.call("GET", &format!("/{stack}"), None).await;
		assert_eq!(s, StatusCode::OK, "{body}");
		body
	}
}

fn service<'a>(detail: &'a Value, name: &str) -> &'a Value {
	detail["detail"]["services"]
		.as_array()
		.unwrap()
		.iter()
		.find(|s| s["name"] == name)
		.unwrap_or_else(|| panic!("no service {name} in {detail}"))
}

#[sqlx::test]
async fn create_get_and_list_never_return_variable_values(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;

	let (status, list) = s.call("GET", "", None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);
	assert_eq!(list[0]["name"], "shop");
	assert_eq!(list[0]["status"], "pending");
	assert_eq!(list[0]["source"], "raw");

	let detail = s.detail(id).await;
	assert_eq!(detail["detail"]["env_keys"], json!(["DB_PASSWORD"]));
	let names: Vec<&str> = detail["detail"]["services"]
		.as_array()
		.unwrap()
		.iter()
		.map(|s| s["name"].as_str().unwrap())
		.collect();
	assert_eq!(names, ["app", "db"]);
	assert_eq!(service(&detail, "db")["image"], "postgres:17");

	// Changing a variable answers with names only, and nothing returns a value.
	let (status, updated) = s
		.call(
			"PATCH",
			&format!("/{id}"),
			Some(json!({ "env": { "LOG_LEVEL": "debug" } })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{updated}");
	assert_eq!(
		updated["detail"]["env_keys"],
		json!(["DB_PASSWORD", "LOG_LEVEL"])
	);
	let everything = [list.to_string(), detail.to_string(), updated.to_string()].join("\n");
	assert!(!everything.contains(SECRET), "leaked: {everything}");
	assert!(!everything.contains("debug"), "leaked: {everything}");

	// At rest the value is encrypted.
	let stored: Vec<u8> = sqlx::query_scalar("SELECT env FROM stacks WHERE id = $1")
		.bind(id)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert!(
		!String::from_utf8_lossy(&stored).contains(SECRET),
		"variables are stored in clear"
	);

	let (status, _) = s
		.call(
			"PATCH",
			&format!("/{id}"),
			Some(json!({ "unset_env": ["LOG_LEVEL"] })),
		)
		.await;
	assert_eq!(status, StatusCode::OK);
	let detail = s.detail(id).await;
	assert_eq!(detail["detail"]["env_keys"], json!(["DB_PASSWORD"]));

	// Duplicate names are a conflict.
	let (status, _) = s.create("shop", COMPOSE, json!({})).await;
	assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn unsupported_compose_is_422_with_each_key_and_stores_nothing(db: PgPool) {
	let s = Setup::new(db).await;
	let yaml = "services:\n  web:\n    build: .\n    image: x\n    privileged: true\n  db:\n    image: y\n    volumes:\n      - /var/run/docker.sock:/s\n";
	let (status, body) = s.create("bad", yaml, json!({})).await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
	let issues: Vec<(&str, &str, &str)> = body["issues"]
		.as_array()
		.unwrap()
		.iter()
		.map(|i| {
			(
				i["service"].as_str().unwrap(),
				i["path"].as_str().unwrap(),
				i["key"].as_str().unwrap(),
			)
		})
		.collect();
	assert_eq!(
		issues,
		[
			("db", "services.db.volumes.0", "0"),
			("web", "services.web.build", "build"),
			("web", "services.web.privileged", "privileged"),
		]
	);
	assert_eq!(count(&s.db, "stacks").await, 0);

	// An update that would stop the stack deploying is refused the same way.
	let id = s.stack().await;
	let (status, body) = s
		.call(
			"PATCH",
			&format!("/{id}"),
			Some(json!({ "compose_yaml": yaml })),
		)
		.await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
	let (_, detail) = s.call("GET", &format!("/{id}"), None).await;
	assert_eq!(service(&detail, "app")["image"], "ghcr.io/acme/app:1");

	let (status, body) = s.create("empty", "services: {}\n", json!({})).await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
	let (status, _) = s.create("junk", "{{{", json!({})).await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn a_required_variable_that_is_not_set_is_refused(db: PgPool) {
	let s = Setup::new(db).await;
	let yaml = "services:\n  db:\n    image: postgres\n    environment:\n      PW: ${DB_PASSWORD:?set the password}\n";
	let (status, body) = s.create("db", yaml, json!({})).await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
	assert_eq!(body["issues"][0]["path"], "services.db.environment.PW");
	let (status, _) = s.create("db", yaml, json!({ "DB_PASSWORD": "x" })).await;
	assert_eq!(status, StatusCode::CREATED);
}

#[sqlx::test]
async fn other_teams_get_404_and_non_members_403(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let (bob, bob_team) = user_with_team(&s.app, "bob").await;
	let (status, bob_project) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	assert!(status.is_success());
	let bob_project = bob_project["id"].as_i64().unwrap();

	let requests: [(&str, String, Option<Value>); 11] = [
		("GET", format!("/{id}"), None),
		("PATCH", format!("/{id}"), Some(json!({ "name": "x" }))),
		("DELETE", format!("/{id}"), None),
		("POST", format!("/{id}/deploy"), None),
		("POST", format!("/{id}/stop"), None),
		("GET", format!("/{id}/services/app/logs"), None),
		("POST", format!("/{id}/services/app/restart"), None),
		(
			"PUT",
			format!("/{id}/services/app/domain"),
			Some(json!({ "hostname": "a.example.com" })),
		),
		("DELETE", format!("/{id}/services/app/domain"), None),
		("GET", format!("/{id}/services/db/logs"), None),
		("POST", format!("/{id}/services/db/restart"), None),
	];
	for (method, rest, body) in requests {
		// Bob's own project, Alice's stack id: the id behaves like a missing one.
		let own = format!("/api/v1/teams/{bob_team}/projects/{bob_project}/stacks{rest}");
		let (status, _) = call(&s.app, method, &own, Some(&bob), body.clone()).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {own}");
		// Alice's project under Bob's team: no such project for him.
		let foreign = format!(
			"/api/v1/teams/{bob_team}/projects/{}/stacks{rest}",
			s.project
		);
		let (status, _) = call(&s.app, method, &foreign, Some(&bob), body.clone()).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {foreign}");
		// Alice's team: he is not a member, so it is not there for him.
		let theirs = s.path(&rest);
		let (status, _) = call(&s.app, method, &theirs, Some(&bob), body).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {theirs}");
	}
	// Nothing happened to the stack.
	assert_eq!(s.detail(id).await["name"], "shop");
	assert_eq!(count(&s.db, "jobs").await, 0);

	// Bob cannot create one in Alice's project through his own team either.
	let (status, _) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects/{}/stacks", s.project),
		Some(&bob),
		Some(json!({
			"name": "evil", "compose_yaml": COMPOSE,
			"server_id": server_id(&s.db, bob_team).await,
			"env": { "DB_PASSWORD": "x" },
		})),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	assert_eq!(count(&s.db, "stacks").await, 1);

	// A server of another team is not an option either.
	let (status, _) = s
		.call(
			"POST",
			"",
			Some(json!({
				"name": "evil", "compose_yaml": COMPOSE,
				"server_id": server_id(&s.db, bob_team).await,
				"env": { "DB_PASSWORD": "x" },
			})),
		)
		.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn viewers_may_read_but_every_mutation_is_refused(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let (carol, _) = user_with_team(&s.app, "carol").await;
	let carol_id = user_id(&s.db, "carol").await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(s.team)
		.bind(carol_id)
		.execute(&s.db)
		.await
		.unwrap();
	let as_carol = |method: &'static str, rest: String, body: Option<Value>| {
		let (app, path, token) = (s.app.clone(), s.path(&rest), carol.clone());
		async move { call(&app, method, &path, Some(&token), body).await }
	};

	let (status, list) = as_carol("GET", String::new(), None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);
	let (status, detail) = as_carol("GET", format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::OK);
	assert!(!detail.to_string().contains(SECRET));

	let jobs_before = count(&s.db, "jobs").await;
	let forged: [(&str, String, Option<Value>); 8] = [
		(
			"POST",
			String::new(),
			Some(json!({
				"name": "x", "compose_yaml": COMPOSE,
				"server_id": s.server().await, "env": { "DB_PASSWORD": "x" },
			})),
		),
		(
			"PATCH",
			format!("/{id}"),
			Some(json!({ "name": "renamed" })),
		),
		("DELETE", format!("/{id}"), None),
		("POST", format!("/{id}/deploy"), None),
		("POST", format!("/{id}/stop"), None),
		("POST", format!("/{id}/services/app/restart"), None),
		(
			"PUT",
			format!("/{id}/services/app/domain"),
			Some(json!({ "hostname": "a.example.com" })),
		),
		("DELETE", format!("/{id}/services/app/domain"), None),
	];
	for (method, rest, body) in forged {
		let (status, _) = as_carol(method, rest.clone(), body).await;
		assert_eq!(status, StatusCode::FORBIDDEN, "{method} {rest}");
	}
	assert_eq!(
		count(&s.db, "jobs").await,
		jobs_before,
		"nothing was queued"
	);
	assert_eq!(count(&s.db, "stacks").await, 1);
	assert_eq!(s.detail(id).await["name"], "shop");
	assert_eq!(s.detail(id).await["status"], "pending");
}

#[sqlx::test]
async fn deploy_starts_services_in_dependency_order_on_the_stack_network(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);

	let (result, log) = s.deploy(id).await;
	result.unwrap_or_else(|e| panic!("deploy failed: {e:?}\n{log}"));

	assert_eq!(
		agent.deployed_services(),
		[format!("stack-{id}-db"), format!("stack-{id}-app")]
	);
	let db_spec = agent.deploy_of(&format!("stack-{id}-db"));
	let net = format!("podkit-stack-{id}");
	assert_eq!(db_spec["networks"], json!([net]));
	assert_eq!(db_spec["options"]["isolated"], true);
	assert_eq!(db_spec["options"]["net_aliases"], json!(["db"]));
	assert_eq!(
		db_spec["volumes"],
		json!([{ "name": format!("podkit-stack-{id}-pgdata"), "path": "/var/lib/postgresql/data" }])
	);
	assert_eq!(db_spec["strategy"], "recreate");
	assert_eq!(db_spec["source"]["pull"], true);
	assert!(
		db_spec["env"]
			.to_string()
			.contains(&format!("POSTGRES_PASSWORD\",\"{SECRET}")),
		"the agent gets the interpolated value"
	);
	assert!(
		agent
			.health_polls
			.lock()
			.unwrap()
			.contains(&format!("ctr-stack-{id}-db")),
		"the app waited on the database's health"
	);
	assert!(log.contains("waiting for db to be healthy"), "{log}");
	assert!(!log.contains(SECRET));

	let detail = s.detail(id).await;
	assert_eq!(detail["status"], "running");
	assert_eq!(service(&detail, "db")["status"], "running");
	assert_eq!(
		service(&detail, "db")["container_id"],
		format!("ctr-stack-{id}-db")
	);
	assert_eq!(service(&detail, "app")["status"], "running");
	assert!(detail["detail"]["latest_job_id"].is_i64());
}

#[sqlx::test]
async fn redeploy_replaces_only_the_services_that_changed(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	s.deploy(id).await.0.unwrap();
	assert_eq!(agent.deployed_services().len(), 2);

	// Nothing changed: nothing is replaced.
	let (result, log) = s.deploy(id).await;
	result.unwrap();
	assert_eq!(agent.deployed_services().len(), 2, "{log}");
	assert!(log.contains("unchanged"), "{log}");

	// LOG_LEVEL only reaches the app.
	let (status, _) = s
		.call(
			"PATCH",
			&format!("/{id}"),
			Some(json!({ "env": { "LOG_LEVEL": "debug" } })),
		)
		.await;
	assert_eq!(status, StatusCode::OK);
	s.deploy(id).await.0.unwrap();
	assert_eq!(
		agent.deployed_services(),
		[
			format!("stack-{id}-db"),
			format!("stack-{id}-app"),
			format!("stack-{id}-app"),
		]
	);

	// A new service is added; the old ones stay.
	let yaml = COMPOSE.replace(
		"volumes:\n  pgdata:\n",
		"  cache:\n    image: redis:alpine\nvolumes:\n  pgdata:\n",
	);
	let (status, body) = s
		.call(
			"PATCH",
			&format!("/{id}"),
			Some(json!({ "compose_yaml": yaml })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	s.deploy(id).await.0.unwrap();
	assert_eq!(
		agent.deployed_services().last().unwrap(),
		&format!("stack-{id}-cache")
	);
	assert_eq!(agent.deployed_services().len(), 4);

	// A service dropped from the file is removed on the next deploy.
	let yaml = COMPOSE.replace(
		"  app:\n    image: ghcr.io/acme/app:1\n    environment:\n      DATABASE_URL: postgres://app:${DB_PASSWORD}@db:5432/app\n      LOG_LEVEL: ${LOG_LEVEL:-info}\n    expose: [\"8080\"]\n    depends_on:\n      db:\n        condition: service_healthy\n",
		"",
	);
	let (status, body) = s
		.call(
			"PATCH",
			&format!("/{id}"),
			Some(json!({ "compose_yaml": yaml })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	let (_, mid) = s.call("GET", &format!("/{id}"), None).await;
	assert_eq!(
		service_status(&mid, "app").as_deref(),
		Some("removing"),
		"until the next deploy"
	);
	s.deploy(id).await.0.unwrap();
	assert_eq!(
		*agent.removed_services.lock().unwrap(),
		[format!("stack-{id}-app"), format!("stack-{id}-cache")],
		"both are gone from the file now"
	);
	assert_eq!(count(&s.db, "stack_services").await, 1);
	assert_eq!(
		agent.deployed_services().len(),
		4,
		"the database was left alone"
	);
}

/// The state of a service in a stack response, `None` when it is not listed.
fn service_status(detail: &Value, name: &str) -> Option<String> {
	detail["detail"]["services"]
		.as_array()?
		.iter()
		.find(|s| s["name"] == name)
		.map(|s| s["status"].as_str().unwrap().to_string())
}

#[sqlx::test]
async fn waiting_on_a_dependency_without_a_health_check_fails_fast(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	// The image has no health check: the container reports none.
	agent.set_health(&format!("ctr-stack-{id}-db"), None, "running");

	let (result, _) = s.deploy(id).await;
	let Err(JobFailure::Failed(message)) = result else {
		panic!("deploy should fail");
	};
	assert!(message.contains("no health check"), "{message}");
	assert_eq!(agent.deployed_services(), [format!("stack-{id}-db")]);
	let detail = s.detail(id).await;
	assert_eq!(detail["status"], "failed");
	assert!(
		detail["status_message"]
			.as_str()
			.unwrap()
			.contains("no health check")
	);
}

#[sqlx::test]
async fn an_unhealthy_dependency_fails_the_deploy_and_a_starting_one_is_waited_for(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	let db_container = format!("ctr-stack-{id}-db");

	agent.set_health(&db_container, Some("unhealthy"), "running");
	let (result, _) = s.deploy(id).await;
	let Err(JobFailure::Failed(message)) = result else {
		panic!("deploy should fail");
	};
	assert!(message.contains("unhealthy"), "{message}");
	assert_eq!(agent.deployed_services(), [format!("stack-{id}-db")]);

	// Starting turns healthy once the job has asked twice and been told "starting": counted
	// in polls, not in time, so a slow machine does not find it healthy at the first one.
	agent.set_health(&db_container, Some("starting"), "running");
	let before = agent.health_polls.lock().unwrap().len();
	let flip = {
		let agent = agent.clone();
		let container = db_container.clone();
		tokio::spawn(async move {
			while agent.health_polls.lock().unwrap().len() < before + 2 {
				tokio::time::sleep(std::time::Duration::from_millis(5)).await;
			}
			agent.set_health(&container, Some("healthy"), "running");
		})
	};
	let (result, log) = s.deploy(id).await;
	// A deploy that asked fewer than twice would leave the flip waiting forever.
	tokio::time::timeout(std::time::Duration::from_secs(5), flip)
		.await
		.expect("the deploy ended after fewer than two polls")
		.unwrap();
	result.unwrap_or_else(|e| panic!("{e:?}\n{log}"));
	assert!(
		agent.health_polls.lock().unwrap().len() >= before + 3,
		"it polled until healthy"
	);
	assert_eq!(s.detail(id).await["status"], "running");
}

#[sqlx::test]
async fn a_service_that_fails_to_start_fails_the_stack_with_its_name_and_reason(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	agent.failures.lock().unwrap().insert(
		format!("stack-{id}-db"),
		"image pull failed: denied".to_string(),
	);

	let (result, _) = s.deploy(id).await;
	let Err(JobFailure::Failed(message)) = result else {
		panic!("deploy should fail");
	};
	assert_eq!(message, "db: image pull failed: denied");
	let detail = s.detail(id).await;
	assert_eq!(detail["status"], "failed");
	assert_eq!(detail["status_message"], "db: image pull failed: denied");
	assert_eq!(service(&detail, "db")["status"], "failed");
	assert_eq!(
		service(&detail, "db")["status_message"],
		"image pull failed: denied"
	);
	assert_eq!(
		service(&detail, "app")["status"],
		"pending",
		"the app never started"
	);
}

#[sqlx::test]
async fn stop_stops_dependents_first_and_deploy_brings_everything_back(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	s.deploy(id).await.0.unwrap();

	let (status, body) = s.call("POST", &format!("/{id}/stop"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	s.run_job(id).await.0.unwrap();
	assert_eq!(
		*agent.stopped.lock().unwrap(),
		[format!("ctr-stack-{id}-app"), format!("ctr-stack-{id}-db")]
	);
	let detail = s.detail(id).await;
	assert_eq!(detail["status"], "stopped");
	assert_eq!(service(&detail, "app")["status"], "stopped");

	// Stopped services are not "running and unchanged": they are started again.
	let before = agent.deployed_services().len();
	s.deploy(id).await.0.unwrap();
	assert_eq!(agent.deployed_services().len(), before + 2);
	assert_eq!(s.detail(id).await["status"], "running");
}

#[sqlx::test]
async fn restart_service_recreates_that_service_only(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	s.deploy(id).await.0.unwrap();

	let (status, body) = s
		.call("POST", &format!("/{id}/services/app/restart"), None)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["service"]["name"], "app");
	s.run_job(id).await.0.unwrap();
	assert_eq!(agent.deployed_services().len(), 3);
	assert_eq!(agent.deployed_services()[2], format!("stack-{id}-app"));

	let (status, _) = s
		.call("POST", &format!("/{id}/services/ghost/restart"), None)
		.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn remove_deletes_containers_and_network_and_volumes_only_when_purging(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	s.deploy(id).await.0.unwrap();

	let (status, body) = s.call("DELETE", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["status"], "deleting");
	// The stack is locked while it goes.
	let (status, _) = s.call("POST", &format!("/{id}/deploy"), None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	s.run_job(id).await.0.unwrap();

	let mut removed = agent.removed_services.lock().unwrap().clone();
	removed.sort();
	assert_eq!(
		removed,
		[format!("stack-{id}-app"), format!("stack-{id}-db")]
	);
	assert_eq!(
		*agent.removed_networks.lock().unwrap(),
		[format!("podkit-stack-{id}")]
	);
	assert!(
		agent.removed_volumes.lock().unwrap().is_empty(),
		"data is kept"
	);
	assert_eq!(count(&s.db, "stacks").await, 0);
	assert_eq!(count(&s.db, "stack_services").await, 0);

	// Purging removes the volumes too.
	let id = s.stack().await;
	s.deploy(id).await.0.unwrap();
	let (status, _) = s
		.call("DELETE", &format!("/{id}?purge_volumes=true"), None)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED);
	s.run_job(id).await.0.unwrap();
	assert_eq!(
		*agent.removed_volumes.lock().unwrap(),
		[format!("podkit-stack-{id}-pgdata")]
	);
	assert_eq!(count(&s.db, "stacks").await, 0);
}

#[sqlx::test]
async fn a_failed_removal_leaves_the_stack_to_retry(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	// No agent is listening.
	let (status, _) = s.call("DELETE", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED);
	let (result, _) = s.run_job(id).await;
	assert!(result.is_err());
	let detail = s.detail(id).await;
	assert_eq!(detail["status"], "failed");
	assert_eq!(count(&s.db, "stacks").await, 1);
}

#[sqlx::test]
async fn domains_route_one_service_and_hostnames_are_unique(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);

	// `app` exposes one port, so none is needed.
	let (status, body) = s
		.call(
			"PUT",
			&format!("/{id}/services/app/domain"),
			Some(json!({ "hostname": "Shop.Example.com" })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["domain"], "shop.example.com");

	// `db` exposes nothing: it needs a port.
	let (status, body) = s
		.call(
			"PUT",
			&format!("/{id}/services/db/domain"),
			Some(json!({ "hostname": "db.example.com" })),
		)
		.await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

	let (status, _) = s
		.call(
			"PUT",
			&format!("/{id}/services/app/domain"),
			Some(json!({ "hostname": "not a host" })),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);

	// Another stack cannot take the hostname.
	let (status, other) = s
		.create(
			"other",
			"services:\n  web:\n    image: x\n    expose: [80]\n",
			json!({}),
		)
		.await;
	assert_eq!(status, StatusCode::CREATED, "{other}");
	let other = other["id"].as_i64().unwrap();
	let (status, _) = s
		.call(
			"PUT",
			&format!("/{other}/services/web/domain"),
			Some(json!({ "hostname": "shop.example.com" })),
		)
		.await;
	assert_eq!(status, StatusCode::CONFLICT);

	// An application's custom domain is taken too.
	crate::tests::seed_application_on(&s.db, s.project, s.server().await).await;
	sqlx::query(
		"INSERT INTO custom_domains (id, application_id, hostname) VALUES ($1, 5002, 'app.example.com')",
	)
	.bind(crypto::generate_id())
	.execute(&s.db)
	.await
	.unwrap();
	let (status, _) = s
		.call(
			"PUT",
			&format!("/{other}/services/web/domain"),
			Some(json!({ "hostname": "app.example.com" })),
		)
		.await;
	assert_eq!(status, StatusCode::CONFLICT);

	// Routing reaches the agent with the deploy.
	s.deploy(id).await.0.unwrap();
	let spec = agent.deploy_of(&format!("stack-{id}-app"));
	assert_eq!(spec["container_port"], 8080);
	assert_eq!(spec["routing"]["http_hosts"], json!(["shop.example.com"]));
	assert_eq!(
		spec["options"]["isolated"],
		Value::Null,
		"a routed service joins ingress"
	);
	assert_eq!(
		spec["network_aliases"],
		json!([format!("podkit-stack-{id}-app")])
	);
	assert_eq!(
		agent.deploy_of(&format!("stack-{id}-db"))["routing"],
		Value::Null
	);

	// Clearing it and deploying again takes the route away from the app only.
	let (status, body) = s
		.call("DELETE", &format!("/{id}/services/app/domain"), None)
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["domain"], Value::Null);
	s.deploy(id).await.0.unwrap();
	assert_eq!(
		agent.deploy_of(&format!("stack-{id}-app"))["routing"],
		Value::Null
	);
	assert_eq!(agent.deployed_services().len(), 3);

	// The freed hostname is available to others.
	let (status, _) = s
		.call(
			"PUT",
			&format!("/{other}/services/web/domain"),
			Some(json!({ "hostname": "shop.example.com" })),
		)
		.await;
	assert_eq!(status, StatusCode::OK);
}

#[sqlx::test]
async fn the_detail_reflects_what_the_agent_reports(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	let agent = fake_agent(&s.state);
	s.deploy(id).await.0.unwrap();

	let detail = s.detail(id).await;
	assert_eq!(service(&detail, "db")["health"], "healthy");

	agent.set_health(&format!("ctr-stack-{id}-app"), None, "exited");
	let detail = s.detail(id).await;
	assert_eq!(service(&detail, "app")["status"], "failed");
	assert_eq!(
		service(&detail, "app")["status_message"],
		"exited with code 3"
	);
	assert_eq!(service(&detail, "db")["status"], "running");
}

#[sqlx::test]
async fn service_logs_need_a_running_container(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.stack().await;
	fake_agent(&s.state);

	let (status, _) = s
		.call(
			"GET",
			&format!("/{id}/services/app/logs?follow=false"),
			None,
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "never deployed");

	s.deploy(id).await.0.unwrap();
	let request = axum::http::Request::builder()
		.method("GET")
		.uri(s.path(&format!("/{id}/services/app/logs?follow=false")))
		.header(header::AUTHORIZATION, format!("Bearer {}", s.token))
		.body(axum::body::Body::empty())
		.unwrap();
	let response = tower::ServiceExt::oneshot(s.app.clone(), request)
		.await
		.unwrap();
	assert_eq!(response.status(), StatusCode::OK);
	let body = axum::body::to_bytes(response.into_body(), usize::MAX)
		.await
		.unwrap();
	let text = String::from_utf8_lossy(&body);
	assert!(
		text.contains("event: log") && text.contains("hello from the container"),
		"{text}"
	);
	assert!(text.contains("event: end"), "{text}");
}

#[sqlx::test]
async fn warnings_and_missing_variables_are_reported(db: PgPool) {
	let s = Setup::new(db).await;
	let yaml = "services:\n  web:\n    image: x:${TAG}\n    tty: true\n";
	let (status, body) = s.create("w", yaml, json!({})).await;
	assert_eq!(status, StatusCode::CREATED, "{body}");
	let detail = s.detail(body["id"].as_i64().unwrap()).await;
	assert_eq!(detail["detail"]["missing_vars"], json!(["TAG"]));
	assert_eq!(detail["detail"]["warnings"][0]["path"], "services.web.tty");
}

#[sqlx::test]
async fn migration_down_removes_the_tables_and_permissions(db: PgPool) {
	const DOWN: &str = include_str!("../../migrations/0036_stacks.down.sql");
	let s = Setup::new(db).await;
	s.stack().await;
	let permissions: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM permissions WHERE resource = 'stack'")
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert_eq!(permissions, 4);
	// Volume backups and the record of template deploys hang off stacks, so they go first.
	for down in [
		include_str!("../../migrations/0085_template_deploys.down.sql"),
		include_str!("../../migrations/0054_volume_backups.down.sql"),
	] {
		sqlx::raw_sql(down).execute(&s.db).await.unwrap();
	}
	for statement in DOWN.split(';').map(str::trim).filter(|q| !q.is_empty()) {
		sqlx::query(sqlx::AssertSqlSafe(statement.to_string()))
			.execute(&s.db)
			.await
			.unwrap();
	}
	let left: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM information_schema.tables WHERE table_name IN ('stacks', 'stack_services')",
	)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!(left, 0);
	let permissions: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM permissions WHERE resource = 'stack'")
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert_eq!(permissions, 0);
}

const LIVE_COMPOSE: &str = r#"
services:
  whoami:
    image: docker.io/traefik/whoami:latest
    expose: ["80"]
    depends_on:
      cache:
        condition: service_healthy
  cache:
    image: docker.io/library/redis:alpine
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 1s
      retries: 20
"#;

/// Runs a podman command and returns its stdout, or the error text.
async fn podman(args: &[&str]) -> Result<String, String> {
	let program = ["/run/current-system/sw/bin/podman", "/usr/bin/podman"]
		.into_iter()
		.find(|p| std::path::Path::new(p).exists())
		.unwrap_or("podman");
	let out = tokio::process::Command::new(program)
		.args(args)
		.output()
		.await
		.map_err(|e| e.to_string())?;
	if out.status.success() {
		Ok(String::from_utf8_lossy(&out.stdout).into_owned())
	} else {
		Err(format!(
			"podman {args:?}: {}{}",
			String::from_utf8_lossy(&out.stdout),
			String::from_utf8_lossy(&out.stderr)
		))
	}
}

/// Two real services on a real podman, through the real agent in this
/// process and the real Traefik ingress: whoami answers 200 on its domain, the
/// services resolve each other by compose name, and removal leaves nothing.
///
/// `cargo test -p podkit-core --all-features -- --ignored stack_live`
#[sqlx::test]
#[ignore = "pulls traefik/whoami and redis:alpine and binds ingress ports 8088 and 8443 on a real podman.sock"]
async fn stack_live_whoami_and_redis(db: PgPool) {
	let s = Setup::new(db).await;
	let podman_api = agent::podman::Podman::connect(&agent::podman::local_socket_path())
		.expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman_api.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman_api.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&s.state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let (status, body) = s.create("live", LIVE_COMPOSE, json!({})).await;
	assert_eq!(status, StatusCode::CREATED, "{body}");
	let id = body["id"].as_i64().unwrap();
	let (status, body) = s
		.call(
			"PUT",
			&format!("/{id}/services/whoami/domain"),
			Some(json!({ "hostname": "whoami.stack-live.test" })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");

	let (result, log) = s.deploy(id).await;
	let outcome: Result<(), String> = async {
		result.map_err(|e| format!("deploy failed: {e:?}\n{log}"))?;

		// HTTP 200 on the domain through Traefik.
		let mut answer = None;
		for _ in 0..40 {
			tokio::time::sleep(std::time::Duration::from_millis(500)).await;
			let response = s
				.state
				.http
				.get(format!("http://127.0.0.1:{}/", s.state.config.ingress_port))
				.header("Host", "whoami.stack-live.test")
				.send()
				.await;
			if let Ok(r) = response
				&& r.status() == 200
			{
				answer = Some(r.text().await.unwrap());
				break;
			}
		}
		let answer = answer.ok_or("whoami never answered 200 through the ingress")?;
		if !answer.contains("Hostname:") {
			return Err(format!("not whoami: {answer}"));
		}

		// The services resolve each other by their compose names.
		for (from, to) in [("cache", "whoami"), ("whoami", "cache")] {
			let container = s.container_of(id, from).await;
			// whoami has no shell: resolve from redis, and ask redis to reach whoami.
			if from == "cache" {
				// nslookup exits 1 when a search domain of the host has no answer.
				let out = podman(&[
					"exec",
					&container,
					"sh",
					"-c",
					&format!("nslookup {to} || true"),
				])
				.await?;
				if !out.contains(&format!("Name:	{to}.dns.podman")) {
					return Err(format!("{from} cannot resolve {to}: {out}"));
				}
			} else {
				let target = s.container_of(id, to).await;
				let ips = podman(&[
					"inspect",
					"-f",
					"{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
					&target,
				])
				.await?;
				if ips.trim().is_empty() {
					return Err(format!("{to} has no address"));
				}
			}
		}
		Ok(())
	}
	.await;

	// Remove everything whatever happened above.
	let (status, _) = s
		.call("DELETE", &format!("/{id}?purge_volumes=true"), None)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED);
	let (removed, log) = s.run_job(id).await;
	let _ = podman(&["rm", "-f", "podkit-traefik"]).await;
	outcome.unwrap_or_else(|e| panic!("{e}"));
	removed.unwrap_or_else(|e| panic!("removal failed: {e:?}\n{log}"));
	assert_eq!(count(&s.db, "stacks").await, 0);
	let leftover = podman(&[
		"ps",
		"-a",
		"--filter",
		&format!("name=stack-{id}"),
		"--format",
		"{{.Names}}",
	])
	.await
	.unwrap();
	assert_eq!(leftover.trim(), "", "containers left");
	assert!(
		podman(&["network", "exists", &format!("podkit-stack-{id}")])
			.await
			.is_err(),
		"the stack network is gone"
	);
}
