//! Network tests against a fake agent on the local server's socket.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::IntoResponse;
use axum::routing::{any, post, put};
use proto::GatewayConfig;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

mod live;

use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::tests::{call, member_role, revoke_permission, state, user_with_team};

#[derive(Default)]
struct FakeAgent {
	deploys: Mutex<Vec<Value>>,
	configs: Mutex<Vec<GatewayConfig>>,
	/// Every request to `/v1/networks...`: method, path and JSON body.
	network_calls: Mutex<Vec<(String, String, Value)>>,
	/// Networks that exist on the fake host besides the ones podkit makes.
	host_networks: Mutex<Vec<String>>,
}

impl FakeAgent {
	fn calls(&self, method: &str) -> Vec<(String, Value)> {
		self.network_calls
			.lock()
			.unwrap()
			.iter()
			.filter(|(m, _, _)| m == method)
			.map(|(_, p, b)| (p.clone(), b.clone()))
			.collect()
	}
}

fn network_json(name: &str) -> Value {
	json!({ "name": name, "internal": false, "driver": "bridge", "containers": [] })
}

fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	let (deploy, config, networks, one) =
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
				let events = [
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
			"/v1/networks",
			any(move |method: Method, uri: Uri, body: Bytes| async move {
				let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
				networks.network_calls.lock().unwrap().push((
					method.to_string(),
					uri.path().to_owned(),
					body.clone(),
				));
				if method == Method::GET {
					let mut all = vec![network_json("podkit"), network_json("bridge")];
					all.extend(
						networks
							.host_networks
							.lock()
							.unwrap()
							.iter()
							.map(|n| network_json(n)),
					);
					return axum::Json(Value::Array(all)).into_response();
				}
				let name = body["name"].as_str().unwrap_or_default().to_owned();
				(StatusCode::CREATED, axum::Json(network_json(&name))).into_response()
			}),
		)
		.route(
			"/v1/networks/{name}",
			any(
				move |method: Method, Path(name): Path<String>, uri: Uri| async move {
					one.network_calls.lock().unwrap().push((
						method.to_string(),
						uri.path().to_owned(),
						Value::Null,
					));
					if method == Method::DELETE {
						return StatusCode::NO_CONTENT.into_response();
					}
					let known = name.starts_with("podkit-")
						|| one.host_networks.lock().unwrap().contains(&name);
					if known {
						axum::Json(network_json(&name)).into_response()
					} else {
						(
							StatusCode::NOT_FOUND,
							axum::Json(json!({ "error": "network not found" })),
						)
							.into_response()
					}
				},
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
	/// The project's `production` environment.
	prod: i64,
	staging: i64,
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
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let (status, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "shop", "slug": "shop" })),
		)
		.await;
		assert!(status.is_success(), "{project}");
		let project = project["id"].as_i64().unwrap();
		let (status, staging) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/environments"),
			Some(&token),
			Some(json!({ "name": "Staging", "slug": "staging" })),
		)
		.await;
		assert!(status.is_success(), "{staging}");
		Self {
			app,
			state,
			db,
			agent,
			token,
			team,
			project,
			server: servers[0]["id"].as_i64().unwrap(),
			prod: project,
			staging: staging["id"].as_i64().unwrap(),
		}
	}

	async fn app_in(&self, slug: &str, environment: i64) -> i64 {
		let (status, application) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications",
				self.team, self.project
			),
			Some(&self.token),
			Some(json!({
				"name": slug, "slug": slug, "server_id": self.server,
				"environment_id": environment,
				"repo_url": "https://github.com/acme/shop", "git_ref": "main",
				"container_port": 3000,
			})),
		)
		.await;
		assert!(status.is_success(), "{application}");
		application["id"].as_i64().unwrap()
	}

	/// Deploys the application and returns the spec the agent was sent.
	async fn deploy(&self, application: i64) -> Value {
		let (status, deployment) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications/{application}/deployments",
				self.team, self.project
			),
			Some(&self.token),
			None,
		)
		.await;
		assert_eq!(status, StatusCode::ACCEPTED, "{deployment}");
		let job = jobs::latest_for(
			&self.db,
			JobKind::Deploy,
			deployment["id"].as_i64().unwrap(),
		)
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

	fn servers(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/servers/{}/networks{rest}",
			self.team, self.server
		)
	}

	fn networks(&self, rest: &str) -> String {
		format!("/api/v1/teams/{}/networks{rest}", self.team)
	}

	async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, uri, Some(&self.token), body).await
	}

	async fn create_network(&self, name: &str, internal: bool) -> i64 {
		let (status, network) = self
			.send(
				"POST",
				&self.servers(""),
				Some(json!({ "name": name, "internal": internal })),
			)
			.await;
		assert_eq!(status, StatusCode::CREATED, "{network}");
		network["id"].as_i64().unwrap()
	}

	async fn attach(&self, network: i64, application: i64) -> StatusCode {
		self.send(
			"POST",
			&self.networks(&format!("/{network}/members")),
			Some(json!({ "kind": "application", "id": application })),
		)
		.await
		.0
	}
}

fn attach_names(spec: &Value) -> Vec<String> {
	spec["attach"]
		.as_array()
		.map(|a| {
			a.iter()
				.map(|n| n["name"].as_str().unwrap().to_owned())
				.collect()
		})
		.unwrap_or_default()
}

#[sqlx::test]
async fn networks_deploy_spec_lists_the_environment_network_and_not_the_other(db: PgPool) {
	let s = Setup::new(db).await;
	let prod = s.app_in("web", s.prod).await;
	let staging = s.app_in("web-staging", s.staging).await;

	let spec = s.deploy(prod).await;
	assert_eq!(attach_names(&spec), [format!("podkit-env-{}", s.prod)]);
	assert!(spec["routing"].is_object(), "a routed app");

	let spec = s.deploy(staging).await;
	let names = attach_names(&spec);
	assert_eq!(names, [format!("podkit-env-{}", s.staging)]);
	assert!(!names.contains(&format!("podkit-env-{}", s.prod)));
	assert!(!names.contains(&"podkit".to_owned()));

	let (_, nets) = s.send("GET", &s.servers(""), None).await;
	assert_eq!(nets.as_array().unwrap().len(), 2, "{nets}");
	for network in nets.as_array().unwrap() {
		assert_eq!(network["kind"], "environment");
		assert_eq!(network["members"].as_array().unwrap().len(), 1);
	}
}

#[sqlx::test]
async fn networks_a_database_keeps_its_alias_on_the_environment_network(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, database) = s
		.send(
			"POST",
			&format!("/api/v1/teams/{}/projects/{}/databases", s.team, s.project),
			Some(json!({
				"name": "main", "engine": "postgres", "server_id": s.server,
				"environment_id": s.staging,
			})),
		)
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{database}");
	let id = database["id"].as_i64().unwrap();
	let job = jobs::latest_for(&s.db, JobKind::DatabaseProvision, id)
		.await
		.unwrap()
		.expect("a provision job");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	crate::databases::run::run(&s.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap();
	let spec = s.agent.deploys.lock().unwrap().last().unwrap().clone();
	assert_eq!(spec["service"], format!("db-{id}"));
	assert_eq!(
		spec["attach"][0],
		json!({ "name": format!("podkit-env-{}", s.staging), "aliases": [format!("podkit-db-{id}")] })
	);
}

#[sqlx::test]
async fn networks_user_network_joins_the_spec_with_its_alias_after_attach(db: PgPool) {
	let s = Setup::new(db).await;
	let (web, api) = (
		s.app_in("web", s.prod).await,
		s.app_in("api", s.staging).await,
	);
	let net = s.create_network("Bridge", true).await;
	assert_eq!(s.attach(net, web).await, StatusCode::NO_CONTENT);
	assert_eq!(s.attach(net, api).await, StatusCode::NO_CONTENT);

	let created = s.agent.calls("POST");
	assert_eq!(created.len(), 1, "{created:?}");
	assert_eq!(created[0].1["internal"], true);
	assert!(
		created[0].1["name"]
			.as_str()
			.unwrap()
			.starts_with("podkit-net-")
	);
	let user_net = created[0].1["name"].as_str().unwrap().to_owned();

	let spec = s.deploy(web).await;
	assert_eq!(
		attach_names(&spec),
		[format!("podkit-env-{}", s.prod), user_net.clone()]
	);
	assert_eq!(spec["attach"][1]["aliases"], json!(["web"]));
	assert_eq!(spec["attach"][1]["internal"], true);
	let spec = s.deploy(api).await;
	assert_eq!(
		attach_names(&spec),
		[format!("podkit-env-{}", s.staging), user_net]
	);

	// Detached, the next deploy no longer lists it.
	let (status, _) = s
		.send(
			"DELETE",
			&s.networks(&format!("/{net}/members/application/{web}")),
			None,
		)
		.await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert_eq!(attach_names(&s.deploy(web).await).len(), 1);
}

#[sqlx::test]
async fn networks_deleting_one_with_members_is_refused(db: PgPool) {
	let s = Setup::new(db).await;
	let web = s.app_in("web", s.prod).await;
	let net = s.create_network("Shared", false).await;
	assert_eq!(s.attach(net, web).await, StatusCode::NO_CONTENT);

	let (status, body) = s
		.send("DELETE", &s.networks(&format!("/{net}")), None)
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	assert!(s.agent.calls("DELETE").is_empty(), "Podman was not asked");
	let (_, view) = s.send("GET", &s.networks(&format!("/{net}")), None).await;
	assert_eq!(view["members"].as_array().unwrap().len(), 1);

	s.send(
		"DELETE",
		&s.networks(&format!("/{net}/members/application/{web}")),
		None,
	)
	.await;
	let (status, _) = s
		.send("DELETE", &s.networks(&format!("/{net}")), None)
		.await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert_eq!(s.agent.calls("DELETE").len(), 1);
	let (status, _) = s.send("GET", &s.networks(&format!("/{net}")), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn networks_an_environment_network_cannot_be_deleted_or_joined_by_hand(db: PgPool) {
	let s = Setup::new(db).await;
	let web = s.app_in("web", s.prod).await;
	s.deploy(web).await;
	let (_, nets) = s.send("GET", &s.servers(""), None).await;
	let env_net = nets[0]["id"].as_i64().unwrap();
	let other = s.app_in("other", s.staging).await;

	let (status, _) = s
		.send("DELETE", &s.networks(&format!("/{env_net}")), None)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	// Only a stack may be attached to an environment network; an app of
	// another environment never.
	assert_eq!(s.attach(env_net, other).await, StatusCode::BAD_REQUEST);
	assert_eq!(s.attach(env_net, web).await, StatusCode::BAD_REQUEST);
	let (status, _) = s
		.send(
			"DELETE",
			&s.networks(&format!("/{env_net}/members/application/{web}")),
			None,
		)
		.await;
	assert_eq!(
		status,
		StatusCode::BAD_REQUEST,
		"an app stays on its network"
	);
}

#[sqlx::test]
async fn networks_another_team_gets_404_everywhere(db: PgPool) {
	let s = Setup::new(db).await;
	let web = s.app_in("web", s.prod).await;
	let net = s.create_network("Mine", false).await;
	let (bob, bob_team) = user_with_team(&s.app, "bob").await;

	// Bob names his own team with Alice's ids.
	let theirs = |rest: &str| format!("/api/v1/teams/{bob_team}/networks{rest}");
	for (method, uri, body) in [
		("GET", theirs(&format!("/{net}")), None),
		(
			"PATCH",
			theirs(&format!("/{net}")),
			Some(json!({ "name": "x" })),
		),
		("DELETE", theirs(&format!("/{net}")), None),
		(
			"POST",
			theirs(&format!("/{net}/members")),
			Some(json!({ "kind": "application", "id": web })),
		),
		("GET", theirs(&format!("/of/application/{web}")), None),
		(
			"GET",
			format!("/api/v1/teams/{bob_team}/servers/{}/networks", s.server),
			None,
		),
		(
			"POST",
			format!(
				"/api/v1/teams/{bob_team}/servers/{}/networks/import",
				s.server
			),
			Some(json!({ "podman_name": "legacy" })),
		),
		(
			"POST",
			format!(
				"/api/v1/teams/{bob_team}/servers/{}/networks/migrate",
				s.server
			),
			None,
		),
	] {
		let (status, body) = call(&s.app, method, &uri, Some(&bob), body).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
	}
	// Alice's own team is untouched.
	let (_, view) = s.send("GET", &s.networks(&format!("/{net}")), None).await;
	assert_eq!(view["name"], "Mine");
}

#[sqlx::test]
async fn networks_import_validates_that_the_network_exists(db: PgPool) {
	let s = Setup::new(db).await;
	s.agent
		.host_networks
		.lock()
		.unwrap()
		.push("legacy".to_owned());

	let (status, found) = s.send("GET", &s.servers("/scan"), None).await;
	assert_eq!(status, StatusCode::OK);
	let names: Vec<_> = found
		.as_array()
		.unwrap()
		.iter()
		.map(|n| n["name"].as_str().unwrap())
		.collect();
	assert_eq!(names, ["legacy"], "podkit and bridge are not offered");

	let (status, body) = s
		.send(
			"POST",
			&s.servers("/import"),
			Some(json!({ "podman_name": "missing" })),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(body["error"].as_str().unwrap().contains("missing"));

	for refused in [
		"podkit",
		"bridge",
		"host",
		"podkit-env-1",
		"podkit-net-2",
		"a b",
	] {
		let (status, _) = s
			.send(
				"POST",
				&s.servers("/import"),
				Some(json!({ "podman_name": refused })),
			)
			.await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
	}

	let (status, network) = s
		.send(
			"POST",
			&s.servers("/import"),
			Some(json!({ "podman_name": "legacy", "name": "Legacy net" })),
		)
		.await;
	assert_eq!(status, StatusCode::CREATED, "{network}");
	assert_eq!(network["imported"], true);
	assert_eq!(network["podman_name"], "legacy");
	let (status, _) = s
		.send(
			"POST",
			&s.servers("/import"),
			Some(json!({ "podman_name": "legacy" })),
		)
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "imported once");
	let (_, found) = s.send("GET", &s.servers("/scan"), None).await;
	assert!(found.as_array().unwrap().is_empty());

	// Forgetting an imported network leaves Podman's copy alone.
	let id = network["id"].as_i64().unwrap();
	let (status, _) = s.send("DELETE", &s.networks(&format!("/{id}")), None).await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert!(s.agent.calls("DELETE").is_empty());
}

#[sqlx::test]
async fn networks_names_are_validated_and_unique_per_server(db: PgPool) {
	let s = Setup::new(db).await;
	for bad in ["", "   ", "!!!", &"x".repeat(61)] {
		let (status, _) = s
			.send("POST", &s.servers(""), Some(json!({ "name": bad })))
			.await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
	}
	s.create_network("Backend", false).await;
	let (status, _) = s
		.send("POST", &s.servers(""), Some(json!({ "name": "backend" })))
		.await;
	assert_eq!(status, StatusCode::CONFLICT);
	assert_eq!(
		s.agent.calls("POST").len(),
		1,
		"only the first reached Podman"
	);

	let net = {
		let (_, nets) = s.send("GET", &s.servers(""), None).await;
		nets[0]["id"].as_i64().unwrap()
	};
	let (status, renamed) = s
		.send(
			"PATCH",
			&s.networks(&format!("/{net}")),
			Some(json!({ "name": "Data plane" })),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{renamed}");
	assert_eq!(renamed["name"], "Data plane");
}

#[sqlx::test]
async fn networks_changes_need_the_server_update_permission(db: PgPool) {
	let s = Setup::new(db).await;
	let role = member_role(&db_of(&s), s.team, "alice").await;
	let net = s.create_network("Mine", false).await;

	revoke_permission(&s.db, role, "update", "server").await;
	let (status, _) = s
		.send("POST", &s.servers(""), Some(json!({ "name": "Other" })))
		.await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	let (status, _) = s
		.send("DELETE", &s.networks(&format!("/{net}")), None)
		.await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	let (status, _) = s.send("POST", &s.servers("/migrate"), None).await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	let (status, _) = s.send("GET", &s.servers(""), None).await;
	assert_eq!(status, StatusCode::OK, "reading is still allowed");

	revoke_permission(&s.db, role, "read", "server").await;
	let (status, _) = s.send("GET", &s.servers(""), None).await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	let (status, _) = call(&s.app, "GET", &s.servers(""), None, None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "no token, no access");
}

fn db_of(s: &Setup) -> PgPool {
	s.db.clone()
}

#[sqlx::test]
async fn networks_the_gateway_joins_the_networks_of_linked_apps_and_models(db: PgPool) {
	let s = Setup::new(db).await;
	let linked = s.app_in("chat", s.staging).await;
	let _lonely = s.app_in("lonely", s.prod).await;

	// A model of the production environment and one link from a staging app.
	let (files, model) = (crypto::generate_id(), crypto::generate_id());
	sqlx::query(
		"INSERT INTO model_files (id, team_id, server_id, repo, revision, selection,
			total_bytes, downloaded_bytes, status, volume_name)
		VALUES ($1, $2, $3, 'acme/open', 'main', '{\"include\":[],\"files\":[]}', 1, 1, 'ready', $4)",
	)
	.bind(files)
	.bind(s.team)
	.bind(s.server)
	.bind(format!("model-files-{files}"))
	.execute(&s.db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO models (id, project_id, environment_id, server_id, model_files_id, name,
			slug, engine, image, port, model_path, served_name, context_size, status)
		VALUES ($1, $2, $3, $4, $5, 'm', 'm', 'llama_cpu', 'img', 8080, 'q.gguf', 'qwen', 4096,
			'running')",
	)
	.bind(model)
	.bind(s.project)
	.bind(s.prod)
	.bind(s.server)
	.bind(files)
	.execute(&s.db)
	.await
	.unwrap();

	let (status, link) = s
		.send(
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications/{linked}/models",
				s.team, s.project
			),
			Some(json!({ "model_id": model })),
		)
		.await;
	assert_eq!(status, StatusCode::CREATED, "{link}");

	let config = s.agent.configs.lock().unwrap().last().unwrap().clone();
	assert_eq!(
		config.networks,
		[
			format!("podkit-env-{}", s.prod),
			format!("podkit-env-{}", s.staging)
		],
		"the staging app that uses a model and the production model, nothing else"
	);
}

#[sqlx::test]
async fn networks_a_resource_tab_lists_what_it_is_on_and_what_it_could_join(db: PgPool) {
	let s = Setup::new(db).await;
	let web = s.app_in("web", s.prod).await;
	let net = s.create_network("Shared", false).await;

	let (status, view) = s
		.send("GET", &s.networks(&format!("/of/application/{web}")), None)
		.await;
	assert_eq!(status, StatusCode::OK, "{view}");
	assert_eq!(view["joined"].as_array().unwrap().len(), 1);
	assert_eq!(view["joined"][0]["kind"], "environment");
	assert_eq!(view["available"][0]["id"], net);

	assert_eq!(s.attach(net, web).await, StatusCode::NO_CONTENT);
	let (_, view) = s
		.send("GET", &s.networks(&format!("/of/application/{web}")), None)
		.await;
	assert_eq!(view["joined"].as_array().unwrap().len(), 2);
	assert_eq!(view["joined"][1]["aliases"], json!(["web"]));
	assert!(view["available"].as_array().unwrap().is_empty());

	for bad in [
		json!({ "aliases": ["UPPER"] }),
		json!({ "aliases": ["a b"] }),
		json!({ "aliases": ["ok", "x".repeat(64)] }),
	] {
		let mut body = bad;
		body["kind"] = json!("application");
		body["id"] = json!(web);
		let (status, _) = s
			.send("POST", &s.networks(&format!("/{net}/members")), Some(body))
			.await;
		assert_eq!(status, StatusCode::BAD_REQUEST);
	}
	let (status, _) = s.send("GET", &s.networks("/of/nothing/1"), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn networks_a_resource_takes_its_memberships_with_it(db: PgPool) {
	let s = Setup::new(db).await;
	let web = s.app_in("web", s.prod).await;
	let net = s.create_network("Shared", false).await;
	s.attach(net, web).await;
	s.deploy(web).await;
	assert_eq!(
		crate::tests::count(&s.db, "resource_networks").await,
		2,
		"its environment's network and the user's"
	);
	sqlx::query("DELETE FROM applications WHERE id = $1")
		.bind(web)
		.execute(&s.db)
		.await
		.unwrap();
	assert_eq!(crate::tests::count(&s.db, "resource_networks").await, 0);
}

#[test]
fn networks_container_names_map_back_to_resources() {
	use super::ResourceKind::{Application, Database, Model};
	use super::service::parse_container;
	assert_eq!(parse_container("podkit-app-12-34"), Some((Application, 12)));
	assert_eq!(parse_container("podkit-db-5-6"), Some((Database, 5)));
	assert_eq!(parse_container("podkit-model-7-8"), Some((Model, 7)));
	for no in [
		"podkit-traefik",
		"podkit-gateway",
		"podkit-stack-1-web-2",
		"podkit-app-x-1",
		"podkit-app-1",
		"podkit-app-1-x",
		"other-app-1-2",
	] {
		assert_eq!(parse_container(no), None, "{no}");
	}
}

#[test]
fn networks_slugs_and_aliases() {
	use super::{slugify, valid_alias};
	assert_eq!(slugify("My Backend!").as_deref(), Some("my-backend"));
	assert_eq!(slugify("  --a--b-- ").as_deref(), Some("a-b"));
	assert_eq!(slugify("!!!"), None);
	assert!(valid_alias("db.internal_1-a"));
	assert!(!valid_alias("Db") && !valid_alias("") && !valid_alias("a b"));
}
