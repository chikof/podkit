//! Private access through Tailscale: the sidecar's deploy request, the write-only auth key, the
//! jobs that start and remove it (against a fake agent), Headscale URLs, tenancy and the pages.
//! `cargo test -p server --all-features -- tailscale`
//! Joining a real tailnet needs an account, so that test is `#[ignore]`d: see `docs/tailscale.md`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

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

use super::run;
use super::spec::{self, Node, Sidecar};
use crate::ids::ApplicationId;
use crate::jobs::{self, JobFailure, JobKind};
use crate::state::AppState;
use crate::tests::{call, count, state, user_with_team};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};

const KEY: &str = "tskey-auth-kABC123CNTRL-0123456789abcdefghijklmnopqrstuvwx";

// --- the sidecar's deploy request ----------------------------------------------------------------

fn sidecar<'a>(
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

fn env<'a>(spec: &'a proto::DeploySpec, key: &str) -> Option<&'a str> {
	spec.env
		.iter()
		.find(|(k, _)| k == key)
		.map(|(_, v)| v.as_str())
}

#[test]
fn tailscale_sidecar_spec_joins_the_network_keeps_state_on_a_volume_and_serves_the_app() {
	let spec = spec::build_spec(&sidecar(None, &[], "shop"));

	// The official image; userspace networking needs no capabilities.
	assert_eq!(spec.service, "ts-app-7");
	assert!(matches!(
		&spec.source,
		proto::Source::Image { reference, .. } if reference == "docker.io/tailscale/tailscale:stable"
	));
	assert_eq!(env(&spec, "TS_USERSPACE"), Some("true"));
	assert_eq!(env(&spec, "TS_HOSTNAME"), Some("shop"));
	assert_eq!(env(&spec, "TS_AUTHKEY"), Some(KEY));
	assert_eq!(env(&spec, "TS_STATE_DIR"), Some("/var/lib/tailscale"));
	assert_eq!(env(&spec, "TS_AUTH_ONCE"), Some("true"));
	assert_eq!(env(&spec, "TS_SERVE_CONFIG"), Some("/config/serve.json"));
	// Nothing extra for the Tailscale service itself.
	assert_eq!(env(&spec, "TS_EXTRA_ARGS"), None);

	// On the application's network, with no public route and no published port.
	assert_eq!(spec.attach.len(), 1);
	assert_eq!(spec.attach[0].name, "pk-env-9");
	assert!(spec.routing.is_none());
	assert!(spec.container_port.is_none());
	assert!(spec.published_ports.is_empty());

	// The node's identity survives a restart: it is on a named volume.
	assert_eq!(spec.volumes.len(), 1);
	assert_eq!(spec.volumes[0].name, "ts-app-7-state");
	assert_eq!(spec.volumes[0].path, "/var/lib/tailscale");
	assert!(!spec.volumes[0].read_only);

	// The serve config is a file that proxies HTTPS to the application's stable alias.
	assert_eq!(spec.options.files.len(), 1);
	assert_eq!(spec.options.files[0].path, "/config/serve.json");
	let serve: Value = serde_json::from_str(&spec.options.files[0].content).unwrap();
	assert_eq!(serve["TCP"]["443"]["HTTPS"], json!(true));
	assert_eq!(
		serve["Web"]["${TS_CERT_DOMAIN}:443"]["Handlers"]["/"]["Proxy"],
		json!("http://pk-app-7:3000")
	);
	// The state volume has one writer at a time.
	assert!(matches!(spec.strategy, proto::Strategy::Recreate));
}

#[test]
fn tailscale_sidecar_passes_the_login_server_and_tags_as_extra_args() {
	let tags = vec!["tag:podkit".to_owned(), "tag:web".to_owned()];
	let spec = spec::build_spec(&sidecar(Some("https://hs.example.com"), &tags, "shop"));
	assert_eq!(
		env(&spec, "TS_EXTRA_ARGS"),
		Some("--login-server=https://hs.example.com --advertise-tags=tag:podkit,tag:web")
	);
	let only_tags = spec::build_spec(&sidecar(None, &tags, "shop"));
	assert_eq!(
		env(&only_tags, "TS_EXTRA_ARGS"),
		Some("--advertise-tags=tag:podkit,tag:web")
	);
}

#[test]
fn tailscale_headscale_login_server_urls_are_normalized_or_refused() {
	let ok = |s: &str| spec::normalize_login_server(s).unwrap();
	// Empty means the Tailscale service itself.
	assert_eq!(ok(""), None);
	assert_eq!(ok("   "), None);
	assert_eq!(
		ok(" https://hs.example.com/ ").as_deref(),
		Some("https://hs.example.com")
	);
	// A homelab Headscale is often plain HTTP on a private address, with a port or a path.
	assert_eq!(
		ok("http://192.168.1.5:8080").as_deref(),
		Some("http://192.168.1.5:8080")
	);
	assert_eq!(
		ok("https://example.com/headscale/").as_deref(),
		Some("https://example.com/headscale")
	);
	for bad in [
		"hs.example.com",
		"ftp://hs.example.com",
		"https://",
		"https://user:pw@hs.example.com",
		"https://hs.example.com/?x=1",
		"https://hs example.com",
		"https://hs.example.com --advertise-exit-node",
		"javascript:alert(1)",
	] {
		assert!(
			spec::normalize_login_server(bad).is_err(),
			"{bad:?} should be refused"
		);
	}
}

#[test]
fn tailscale_tags_are_parsed_normalized_and_bounded() {
	assert_eq!(
		spec::parse_tags("tag:podkit, web  tag:podkit\nops").unwrap(),
		["tag:podkit", "tag:web", "tag:ops"]
	);
	assert!(spec::parse_tags("").unwrap().is_empty());
	for bad in ["Tag:Upper", "tag:a;rm", "tag:", "tag:1abc", "tag:a_b"] {
		assert!(spec::parse_tags(bad).is_err(), "{bad:?} should be refused");
	}
	let many = (0..11)
		.map(|n| format!("t{n}"))
		.collect::<Vec<_>>()
		.join(" ");
	assert!(spec::parse_tags(&many).is_err());
}

#[test]
fn tailscale_node_names_and_auth_keys_are_checked() {
	assert!(spec::valid_hostname("shop-2"));
	for bad in ["", "-a", "a-", "Shop", "a.b", "a_b", &"a".repeat(64)] {
		assert!(!spec::valid_hostname(bad), "{bad:?}");
	}
	assert_eq!(spec::default_hostname("My_App.v2"), "my-app-v2");
	assert!(spec::check_auth_key(KEY).is_ok());
	// Headscale pre-auth keys are plain hex.
	assert!(spec::check_auth_key("0123456789abcdef0123456789abcdef").is_ok());
	assert!(spec::check_auth_key("two words").is_err());
	assert!(spec::check_auth_key("").is_err());
}

#[test]
fn tailscale_status_output_is_read() {
	assert_eq!(
		spec::parse_status(
			r#"{"BackendState":"Running","Self":{"DNSName":"shop.tail1234.ts.net."}}"#
		),
		Some(Node::Running {
			dns_name: "shop.tail1234.ts.net".to_owned()
		})
	);
	assert_eq!(
		spec::parse_status(r#"{"BackendState":"NeedsLogin"}"#),
		Some(Node::NeedsLogin)
	);
	assert_eq!(
		spec::parse_status(r#"{"BackendState":"Starting"}"#),
		Some(Node::Other("Starting".to_owned()))
	);
	assert_eq!(spec::parse_status("not json"), None);
}

// --- a fake agent ------------------------------------------------------------------------------

#[derive(Default)]
struct FakeAgent {
	deploys: Mutex<Vec<Value>>,
	/// Every call that changes the sidecar, in order: `exec <container> <args…>`,
	/// `delete service <name>`, `delete volume <name>`.
	calls: Mutex<Vec<String>>,
	/// What `tailscale status --json` prints.
	status: Mutex<String>,
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

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	agent: Arc<FakeAgent>,
	token: String,
	team: i64,
	project: i64,
	application: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let agent = fake_agent(&state);
		let app = crate::routes::routes(state.clone()).await.unwrap();
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

	async fn project_and_app(app: &Router, token: &str, team: i64, slug: &str) -> (i64, i64) {
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

	fn settings_path(&self) -> String {
		format!("/api/v1/teams/{}/tailscale", self.team)
	}

	fn exposure_path(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/tailscale",
			self.team, self.project, self.application
		)
	}

	async fn put_settings(&self, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"PUT",
			&self.settings_path(),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	async fn store_key(&self) {
		let (status, body) = self.put_settings(json!({ "auth_key": KEY })).await;
		assert_eq!(status, StatusCode::OK, "{body}");
	}

	async fn enable(&self, body: Value) -> (StatusCode, Value) {
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
	async fn run_job(&self, kind: JobKind) -> Result<(), JobFailure> {
		let job = jobs::latest_for(&self.db, kind, self.application)
			.await
			.unwrap()
			.expect("the job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let result = run::run(&self.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		result
	}

	async fn status(&self) -> Value {
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

// --- the auth key is write-only ----------------------------------------------------------------

#[sqlx::test]
async fn tailscale_auth_key_is_write_only_and_stored_encrypted(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s
		.put_settings(json!({
			"auth_key": KEY, "login_server": "https://hs.example.com/",
			"tags": "tag:podkit web",
		}))
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(
		body,
		json!({ "auth_key_set": true, "login_server": "https://hs.example.com",
			"tags": ["tag:podkit", "tag:web"] })
	);

	let (_, read) = call(&s.app, "GET", &s.settings_path(), Some(&s.token), None).await;
	assert_eq!(read["auth_key_set"], json!(true));
	assert!(!read.to_string().contains("tskey"));

	// At rest it is a blob that is not the key, and only the secret box opens it.
	let blob: Vec<u8> = sqlx::query_scalar("SELECT auth_key FROM tailscale_settings")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert!(!blob.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
	assert_eq!(s.state.secrets.decrypt(&blob).unwrap(), KEY);

	// Saving the other settings with an empty key keeps it.
	let (status, body) = s.put_settings(json!({ "tags": "tag:other" })).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["auth_key_set"], json!(true));
	assert_eq!(body["tags"], json!(["tag:other"]));
	assert_eq!(body["login_server"], Value::Null);

	// It reaches neither the audit log nor the page.
	let audit: Vec<String> = sqlx::query_scalar("SELECT diff::text FROM audit_events")
		.fetch_all(&s.db)
		.await
		.unwrap();
	assert!(!audit.is_empty());
	assert!(audit.iter().all(|d| !d.contains("tskey")), "{audit:?}");
	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events")
		.fetch_all(&s.db)
		.await
		.unwrap();
	assert!(actions.iter().any(|a| a == "tailscale.settings.update"));

	let cookie = ui_login(&s.app, "alice").await;
	let (status, _, page) = get_html(
		&s.app,
		&format!("/teams/{}/settings/tailscale", s.team),
		Some(&cookie),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_not_contains(&page, "tskey-auth-k");
	assert_contains(page_main(&page), "Stored");
	assert_eq!(page_main(&page).matches("<h1").count(), 1);

	// Clearing forgets it.
	let (status, body) = s
		.put_settings(json!({ "clear_auth_key": true, "tags": "tag:other" }))
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["auth_key_set"], json!(false));
}

#[sqlx::test]
async fn tailscale_settings_refuse_a_bad_login_server_tag_or_key(db: PgPool) {
	let s = Setup::new(db).await;
	for body in [
		json!({ "login_server": "hs.example.com" }),
		json!({ "tags": "tag:Bad!" }),
		json!({ "auth_key": "has spaces" }),
	] {
		let (status, answer) = s.put_settings(body.clone()).await;
		assert!(status.is_client_error(), "{body}: {status} {answer}");
	}
	assert_eq!(count(&s.db, "tailscale_settings").await, 0);
}

// --- enabling, the start job and removal -------------------------------------------------------

#[sqlx::test]
async fn tailscale_enable_needs_a_stored_key_and_a_valid_name(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s.enable(json!({})).await;
	assert!(status.is_client_error(), "{status} {body}");
	assert!(body.to_string().contains("auth key"));
	s.store_key().await;
	let (status, _) = s.enable(json!({ "hostname": "Not A Name" })).await;
	assert!(status.is_client_error());
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
}

#[sqlx::test]
async fn tailscale_start_job_deploys_the_sidecar_and_records_the_node(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.put_settings(json!({ "login_server": "http://10.0.0.2:8080", "tags": "tag:podkit" }))
		.await;
	let (status, body) = s.enable(json!({})).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	// The name defaults to the slug, and the sidecar starts in a job.
	assert_eq!(body["hostname"], json!("shop"));
	assert_eq!(body["status"], json!("starting"));
	assert_eq!(body["url"], Value::Null);

	s.run_job(JobKind::TailscaleStart).await.unwrap();

	let deploys = s.agent.deploys.lock().unwrap().clone();
	assert_eq!(deploys.len(), 1);
	let spec = &deploys[0];
	assert_eq!(spec["service"], json!(format!("ts-app-{}", s.application)));
	let env: Vec<(String, String)> = serde_json::from_value(spec["env"].clone()).unwrap();
	let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
	assert_eq!(get("TS_AUTHKEY"), Some(KEY));
	assert_eq!(get("TS_HOSTNAME"), Some("shop"));
	assert_eq!(
		get("TS_EXTRA_ARGS"),
		Some("--login-server=http://10.0.0.2:8080 --advertise-tags=tag:podkit")
	);
	assert_eq!(spec["attach"].as_array().unwrap().len(), 1);
	assert_eq!(
		spec["volumes"][0]["name"],
		json!(format!("ts-app-{}-state", s.application))
	);

	let exposure = s.status().await;
	assert_eq!(exposure["status"], json!("running"));
	assert_eq!(exposure["url"], json!("https://shop.tail1234.ts.net"));
	// The key never comes back out through the exposure, either.
	assert!(!exposure.to_string().contains("tskey"));

	// The application itself now answers to the alias the sidecar proxies to.
	let (status, deployment) = call(
		&s.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/deployments",
			s.team, s.project, s.application
		),
		Some(&s.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{deployment}");
	let job = jobs::latest_for(&s.db, JobKind::Deploy, deployment["id"].as_i64().unwrap())
		.await
		.unwrap()
		.expect("a deploy job is queued");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	crate::deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap();
	log.finish().await;
	let deploys = s.agent.deploys.lock().unwrap().clone();
	let app_spec = deploys.last().unwrap();
	let alias = format!("pk-app-{}", s.application);
	let attached = app_spec["attach"].as_array().unwrap();
	assert!(
		attached.iter().any(|n| n["aliases"]
			.as_array()
			.is_some_and(|a| a.iter().any(|x| x == &json!(alias)))),
		"{attached:?}"
	);
}

#[sqlx::test]
async fn tailscale_a_refused_auth_key_fails_with_a_reason(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	*s.agent.status.lock().unwrap() = r#"{"BackendState":"NeedsLogin"}"#.to_owned();
	s.enable(json!({})).await;
	let job = jobs::latest_for(&s.db, JobKind::TailscaleStart, s.application)
		.await
		.unwrap()
		.unwrap();
	let mut log = s.state.logs.open(job.id).await.unwrap();
	let app = crate::applications::find_by_id(&s.db, ApplicationId(s.application))
		.await
		.unwrap()
		.unwrap();
	let server = crate::servers::find(&s.db, app.server_id)
		.await
		.unwrap()
		.unwrap();
	let client = s.state.agents.client(&s.state, &server).await.unwrap();
	// The real grace period is 40 seconds; shortened here.
	let result = run::wait_joined(
		&s.state,
		&CancellationToken::new(),
		&mut log,
		app.id,
		&client,
		"side1",
		Duration::from_secs(5),
		Duration::from_millis(5),
		Duration::from_millis(30),
	)
	.await;
	match result {
		Err(JobFailure::Failed(m)) => assert!(m.contains("did not accept the auth key"), "{m}"),
		other => panic!("expected a refused key, got {other:?}"),
	}
}

#[sqlx::test]
async fn tailscale_removal_logs_the_node_out_before_the_sidecar_and_volume_go(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;
	s.run_job(JobKind::TailscaleStart).await.unwrap();
	s.agent.calls.lock().unwrap().clear();

	let (status, body) = call(&s.app, "DELETE", &s.exposure_path(), Some(&s.token), None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["exposure"]["status"], json!("removing"));

	s.run_job(JobKind::TailscaleRemove).await.unwrap();
	let calls = s.agent.calls.lock().unwrap().clone();
	let id = s.application;
	assert_eq!(
		calls,
		[
			"exec side1 tailscale --socket /tmp/tailscaled.sock logout".to_owned(),
			format!("delete service ts-app-{id}"),
			format!("delete volume ts-app-{id}-state"),
		]
	);
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
	assert_eq!(s.status().await, Value::Null);

	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events ORDER BY id")
		.fetch_all(&s.db)
		.await
		.unwrap();
	for expected in ["tailscale.enable", "tailscale.disable", "tailscale.removed"] {
		assert!(
			actions.iter().any(|a| a == expected),
			"{expected} in {actions:?}"
		);
	}
}

#[sqlx::test]
async fn tailscale_deleting_the_application_logs_the_node_out_too(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;
	s.run_job(JobKind::TailscaleStart).await.unwrap();
	s.agent.calls.lock().unwrap().clear();

	let (status, body) = call(
		&s.app,
		"DELETE",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications/{}",
			s.team, s.project, s.application
		),
		Some(&s.token),
		None,
	)
	.await;
	assert!(status.is_success(), "{status} {body}");
	let calls = s.agent.calls.lock().unwrap().clone();
	assert!(
		calls.iter().any(|c| c.ends_with("logout")),
		"the node should be logged out: {calls:?}"
	);
	assert!(calls.iter().any(|c| c.starts_with("delete volume ts-app-")));
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
}

#[sqlx::test]
async fn tailscale_a_failed_start_is_kept_on_the_exposure_and_tells_the_team(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;
	// The key is forgotten between turning it on and the job running.
	sqlx::query("UPDATE tailscale_settings SET auth_key = NULL")
		.execute(&s.db)
		.await
		.unwrap();
	let err = s.run_job(JobKind::TailscaleStart).await.unwrap_err();
	let JobFailure::Failed(message) = &err else {
		panic!("{err:?}")
	};
	assert!(message.contains("no Tailscale auth key"), "{message}");

	let job = jobs::latest_for(&s.db, JobKind::TailscaleStart, s.application)
		.await
		.unwrap()
		.unwrap();
	jobs::fail_target(&s.state, JobKind::TailscaleStart, s.application, &err).await;
	crate::notifications::hooks::job_finished(&s.state, &job, &Err(err)).await;
	let exposure = s.status().await;
	assert_eq!(exposure["status"], json!("failed"));
	assert!(exposure["error"].as_str().unwrap().contains("auth key"));
	let events: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM notification_events WHERE kind = 'tailnet_failed'",
	)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!(events, 1);
}

// --- who may do what, and whose it is ---------------------------------------------------------

#[sqlx::test]
async fn tailscale_other_teams_get_a_404_and_change_nothing(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;

	let (bob_token, bob_team) = user_with_team(&s.app, "bob").await;
	let (bob_project, _) = Setup::project_and_app(&s.app, &bob_token, bob_team, "mine").await;
	let id = s.application;

	// Bob uses alice's application id under his own team and project: a miss.
	let mine =
		format!("/api/v1/teams/{bob_team}/projects/{bob_project}/applications/{id}/tailscale");
	for (method, body) in [
		("GET", None),
		("PUT", Some(json!({ "hostname": "stolen" }))),
		("DELETE", None),
	] {
		let (status, answer) = call(&s.app, method, &mine, Some(&bob_token), body).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method}: {answer}");
	}
	// Under her team: refused, nothing leaked.
	for (method, path, body) in [
		("GET", s.exposure_path(), None),
		(
			"PUT",
			s.exposure_path(),
			Some(json!({ "hostname": "stolen" })),
		),
		("DELETE", s.exposure_path(), None),
		("GET", s.settings_path(), None),
		(
			"PUT",
			s.settings_path(),
			Some(json!({ "auth_key": "tskey-evil" })),
		),
	] {
		let (status, answer) = call(&s.app, method, &path, Some(&bob_token), body).await;
		assert!(
			matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
			"{method} {path}: {status} {answer}"
		);
		assert!(!answer.to_string().contains("shop"));
	}
	let (status, _) = call(&s.app, "GET", &s.settings_path(), None, None).await;
	assert!(status.is_client_error(), "{status}");

	// Bob's own team has its own settings: alice's key is not his.
	let (_, theirs) = call(
		&s.app,
		"GET",
		&format!("/api/v1/teams/{bob_team}/tailscale"),
		Some(&bob_token),
		None,
	)
	.await;
	assert_eq!(theirs["auth_key_set"], json!(false));

	assert_eq!(s.status().await["hostname"], json!("shop"));
	let key: Vec<u8> =
		sqlx::query_scalar("SELECT auth_key FROM tailscale_settings WHERE team_id = $1")
			.bind(s.team)
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert_eq!(s.state.secrets.decrypt(&key).unwrap(), KEY);

	// The pages: his cookie on her application's address is a 404 as well.
	let cookie = ui_login(&s.app, "bob").await;
	let (status, _, _) = get_html(
		&s.app,
		&format!("/teams/{bob_team}/projects/{bob_project}/apps/{id}/tailnet"),
		Some(&cookie),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn tailscale_pages_show_the_card_and_escape_what_they_show(db: PgPool) {
	let s = Setup::new(db).await;
	let cookie = ui_login(&s.app, "alice").await;
	let team = s.team;
	let base = format!(
		"/teams/{team}/projects/{}/apps/{}",
		s.project, s.application
	);
	let domains = format!("{base}/domains");

	// Without a key the card points to Settings.
	let (status, _, page) = get_html(&s.app, &domains, Some(&cookie)).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, "Private access (Tailscale)");
	assert_contains(main, &format!("/teams/{team}/settings/tailscale"));
	assert_eq!(main.matches("<h1").count(), 1);

	// With one it offers a name, and turning it on from the UI queues the job.
	let (status, _, _) = post_signals(
		&s.app,
		&format!("/teams/{team}/settings/tailscale"),
		Some(&cookie),
		&json!({ "ts_key": KEY, "ts_login_server": "", "ts_tags": "tag:podkit" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	let (_, _, page) = get_html(&s.app, &domains, Some(&cookie)).await;
	assert_contains(page_main(&page), "Turn on");
	let (status, _, _) = post_signals(
		&s.app,
		&format!("{base}/tailnet"),
		Some(&cookie),
		&json!({ "tn_hostname": "shop" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(count(&s.db, "tailscale_exposures").await, 1);

	// An error message that came from elsewhere is escaped.
	sqlx::query("UPDATE tailscale_exposures SET status = 'failed', error = '<b>boom</b>'")
		.execute(&s.db)
		.await
		.unwrap();
	let (_, _, page) = get_html(&s.app, &domains, Some(&cookie)).await;
	assert_contains(page_main(&page), "&lt;b&gt;boom&lt;/b&gt;");
	assert_not_contains(page_main(&page), "<b>boom</b>");
	assert_not_contains(&page, "tskey-auth-k");
}

#[sqlx::test]
async fn tailscale_a_plain_member_cannot_change_settings_or_exposure(db: PgPool) {
	let s = Setup::new(db).await;
	let alice = ui_login(&s.app, "alice").await;
	let team = s.team;
	s.store_key().await;

	let creds =
		json!({ "name": "bob", "email": "bob@example.com", "password": "correct horse battery" });
	let (status, _) = call(&s.app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(status.is_success());
	let (status, _, _) = post_signals(
		&s.app,
		&format!("/teams/{team}/members"),
		Some(&alice),
		&json!({ "email": "bob@example.com", "newrole": "2" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	let bob = ui_login(&s.app, "bob").await;

	// The page shows state but no form; a forged POST changes nothing.
	let settings = format!("/teams/{team}/settings/tailscale");
	let (status, _, page) = get_html(&s.app, &settings, Some(&bob)).await;
	assert_eq!(status, StatusCode::OK);
	assert_not_contains(page_main(&page), "ts_key");
	assert_not_contains(&page, "tskey-auth-k");
	post_signals(
		&s.app,
		&settings,
		Some(&bob),
		&json!({ "ts_key": "tskey-forged", "ts_login_server": "https://evil.example", "ts_tags": "" }),
	)
	.await;
	let blob: Vec<u8> = sqlx::query_scalar("SELECT auth_key FROM tailscale_settings")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(s.state.secrets.decrypt(&blob).unwrap(), KEY);
	let server: Option<String> = sqlx::query_scalar("SELECT login_server FROM tailscale_settings")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(server, None);

	let enable = format!(
		"/teams/{team}/projects/{}/apps/{}/tailnet",
		s.project, s.application
	);
	post_signals(
		&s.app,
		&enable,
		Some(&bob),
		&json!({ "tn_hostname": "shop" }),
	)
	.await;
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
}

// --- live --------------------------------------------------------------------------------------

/// Needs a real tailnet: `TS_LIVE_AUTHKEY=tskey-auth-… cargo test -p server --all-features --
/// --ignored tailscale_live`. The sidecar itself is only proven by reaching the app from a
/// tailnet device; the steps are in `docs/tailscale.md`.
#[test]
#[ignore = "needs a Tailscale account: see docs/tailscale.md"]
fn tailscale_live_key_is_well_formed() {
	let key = std::env::var("TS_LIVE_AUTHKEY").expect("set TS_LIVE_AUTHKEY");
	assert!(spec::check_auth_key(&key).is_ok());
}
