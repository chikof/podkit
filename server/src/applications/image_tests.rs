//! Applications that deploy a registry image instead of a git repo
//! (migration 0035). Everything here runs against a fake agent except the
//! `#[ignore]`d live test, which needs a real `podman.sock` and the network.
//!
//! `cargo test -p server --all-features -- image_app`

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use proto::Source;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::applications;
use crate::deployments::{self, Deployment};
use crate::ids::{ApplicationId, DeploymentId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::tests::{call, state, user_with_team};

const DIGEST: &str = "docker.io/traefik/whoami@sha256:0000000000000000000000000000000000000000000000000000000000000001";

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	server: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		Self::with_state(db.clone(), state(db)).await
	}

	async fn with_state(db: PgPool, state: AppState) -> Self {
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
		assert!(s.is_success(), "create project: {s}");
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		Self {
			app,
			state,
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
			server: servers[0]["id"].as_i64().unwrap(),
		}
	}

	fn apps(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications",
			self.team, self.project
		)
	}

	/// The common fields of an application, with `extra` merged over them.
	fn body(&self, slug: &str, extra: &Value) -> Value {
		let mut body = json!({
			"name": slug, "slug": slug, "server_id": self.server, "container_port": 80,
		});
		for (k, v) in extra.as_object().unwrap() {
			body[k] = v.clone();
		}
		body
	}

	async fn create(&self, slug: &str, extra: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"POST",
			&self.apps(),
			Some(&self.token),
			Some(self.body(slug, &extra)),
		)
		.await
	}

	async fn update(&self, id: i64, slug: &str, extra: Value) -> (StatusCode, Value) {
		let mut body = self.body(slug, &extra);
		body.as_object_mut().unwrap().remove("server_id");
		call(
			&self.app,
			"PATCH",
			&format!("{}/{id}", self.apps()),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	async fn stored(&self, id: i64) -> applications::Application {
		applications::find_by_id(&self.db, ApplicationId(id))
			.await
			.unwrap()
			.unwrap()
	}

	/// Queues a deploy the way the API, a restart or a webhook does, and
	/// runs its job the way the worker does.
	async fn deploy(&self, id: i64, reuse: Option<&Deployment>) -> Deployment {
		let app = self.stored(id).await;
		let queued = deployments::queue(&self.state, &app, None, reuse)
			.await
			.unwrap();
		let job = jobs::latest_for(&self.db, JobKind::Deploy, queued.id.0)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log)
			.await
			.unwrap_or_else(|e| panic!("deploy failed: {e:?}"));
		log.finish().await;
		self.deployment(queued.id).await
	}

	async fn deployment(&self, id: DeploymentId) -> Deployment {
		deployments::find_by_id(&self.db, id)
			.await
			.unwrap()
			.unwrap()
	}
}

/// A fake agent on the local server's socket that records every deploy spec
/// and answers like a real one after a pull: with the image's digest.
fn fake_agent(state: &AppState) -> Arc<Mutex<Vec<Value>>> {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let recorder = seen.clone();
	let app = Router::new().route(
		"/v1/deploy",
		post(move |body: Bytes| async move {
			recorder
				.lock()
				.unwrap()
				.push(serde_json::from_slice(&body).unwrap());
			let events = [
				json!({ "type": "log", "stream": "system", "line": "pulling" }),
				json!({ "type": "deployed", "container_id": "c1", "image": DIGEST,
					"commit_sha": null }),
			];
			let body = events.iter().fold(String::new(), |mut out, e| {
				out.push_str(&e.to_string());
				out.push('\n');
				out
			});
			([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
		}),
	);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	seen
}

#[sqlx::test]
async fn image_app_create_with_an_image_and_with_a_repo(db: PgPool) {
	let s = Setup::new(db).await;

	let (status, img) = s
		.create(
			"whoami",
			json!({ "image": "docker.io/traefik/whoami:v1.10" }),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{img}");
	assert_eq!(img["source_kind"], "image");
	assert_eq!(img["image"], "docker.io/traefik/whoami:v1.10");
	assert_eq!(img["repo_url"], Value::Null);
	assert_eq!(img["has_registry_auth"], false);
	let stored = s.stored(img["id"].as_i64().unwrap()).await;
	assert_eq!(stored.source_kind, applications::SourceKind::Image);
	assert_eq!(stored.repo_url, None);

	let (status, git) = s
		.create(
			"site",
			json!({ "repo_url": "https://github.com/traefik/whoami", "git_ref": "master" }),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{git}");
	assert_eq!(git["source_kind"], "git");
	assert_eq!(git["repo_url"], "https://github.com/traefik/whoami");
	assert_eq!(git["image"], Value::Null);
	let stored = s.stored(git["id"].as_i64().unwrap()).await;
	assert_eq!(stored.source_kind, applications::SourceKind::Git);
	assert_eq!(stored.image, None);

	// Both are listed, with their own source.
	let (_, listed) = call(&s.app, "GET", &s.apps(), Some(&s.token), None).await;
	let kinds: Vec<_> = listed
		.as_array()
		.unwrap()
		.iter()
		.map(|a| a["source_kind"].as_str().unwrap())
		.collect();
	assert_eq!(kinds, ["image", "git"]);

	// An image app can be edited without ever naming a repository.
	let id = img["id"].as_i64().unwrap();
	let (status, updated) = s
		.update(id, "whoami", json!({ "image": "traefik/whoami:v1.11" }))
		.await;
	assert_eq!(status, StatusCode::OK, "{updated}");
	assert_eq!(updated["image"], "traefik/whoami:v1.11");
}

#[sqlx::test]
async fn image_app_check_constraint_rejects_mixed_or_missing_sources(db: PgPool) {
	let s = Setup::new(db).await;
	let insert =
		|id: i64, kind: &'static str, repo: Option<&'static str>, image: Option<&'static str>| {
			let (db, project, server) = (s.db.clone(), s.project, s.server);
			async move {
				sqlx::query(
					"INSERT INTO applications
					(id, project_id, server_id, name, slug, source_kind, repo_url, image, container_port)
				 VALUES ($1, $2, $3, 'n', $4, $5, $6, $7, 80)",
				)
				.bind(id)
				.bind(project)
				.bind(server)
				.bind(format!("app-{id}"))
				.bind(kind)
				.bind(repo)
				.bind(image)
				.execute(&db)
				.await
			}
		};
	let repo = Some("https://example.com/r.git");
	let image = Some("alpine:3");

	insert(1, "git", repo, None).await.unwrap();
	insert(2, "image", None, image).await.unwrap();

	for (id, kind, repo, image, why) in [
		(3, "git", repo, image, "a git app with an image"),
		(4, "image", repo, image, "an image app with a repo"),
		(5, "git", None, None, "a git app without a repo"),
		(6, "image", None, None, "an image app without an image"),
		(7, "image", repo, None, "an image app that only has a repo"),
		(8, "git", None, image, "a git app that only has an image"),
		(9, "docker", None, image, "an unknown kind"),
	] {
		let err = insert(id, kind, repo, image).await.expect_err(why);
		assert!(
			err.to_string().contains("applications_source_check"),
			"{why}: {err}"
		);
	}

	// Existing rows are git, and `git` stays the default.
	let kind: String = sqlx::query_scalar(
		"INSERT INTO applications (id, project_id, server_id, name, slug, repo_url, container_port)
		 VALUES (10, $1, $2, 'n', 'plain', 'https://example.com/r.git', 80) RETURNING source_kind",
	)
	.bind(s.project)
	.bind(s.server)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!(kind, "git");
}

#[sqlx::test]
async fn image_app_service_rejects_mixed_missing_and_bad_sources(db: PgPool) {
	let s = Setup::new(db).await;
	let before = crate::tests::count(&s.db, "applications").await;

	let (status, git) = s
		.create("site", json!({ "repo_url": "https://example.com/r.git" }))
		.await;
	assert_eq!(status, StatusCode::OK, "{git}");
	let (status, img) = s.create("pic", json!({ "image": "alpine:3" })).await;
	assert_eq!(status, StatusCode::OK, "{img}");
	let (git_id, img_id) = (git["id"].as_i64().unwrap(), img["id"].as_i64().unwrap());

	let refused = [
		(json!({}), "repo_url or image is required"),
		(
			json!({ "repo_url": "https://example.com/r.git", "image": "alpine:3" }),
			"not both",
		),
		(json!({ "image": "Not A Ref" }), "invalid image reference"),
		(json!({ "image": "-v /:/host" }), "invalid image reference"),
		(
			json!({ "image": "alpine:3", "deploy_key": "KEY" }),
			"only apply to git",
		),
		(
			json!({ "image": "alpine:3", "registry_username": "me" }),
			"go together",
		),
		(
			json!({ "image": "alpine:3", "registry_password": "pw" }),
			"go together",
		),
		(
			json!({ "repo_url": "https://example.com/r.git", "registry_username": "me",
				"registry_password": "pw" }),
			"only apply to image",
		),
	];
	for (extra, want) in refused {
		let (status, body) = s.create("other", extra.clone()).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{extra} {body}");
		assert!(body.to_string().contains(want), "{extra}: {body}");
	}
	assert_eq!(
		crate::tests::count(&s.db, "applications").await,
		before + 2,
		"a refused create inserts nothing"
	);

	// The source type is fixed once the application exists.
	let (status, body) = s
		.update(git_id, "site", json!({ "image": "alpine:3" }))
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(body.to_string().contains("cannot be changed"), "{body}");
	let (status, body) = s
		.update(
			img_id,
			"pic",
			json!({ "repo_url": "https://example.com/r.git" }),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert_eq!(s.stored(img_id).await.repo_url, None);
}

#[sqlx::test]
async fn image_app_registry_credentials_are_write_only(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, created) = s
		.create(
			"private",
			json!({ "image": "ghcr.io/acme/app:1", "registry_username": "robot",
				"registry_password": "hunter2-very-secret" }),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{created}");
	let id = created["id"].as_i64().unwrap();

	let (_, listed) = call(&s.app, "GET", &s.apps(), Some(&s.token), None).await;
	for body in [&created, &listed] {
		let text = body.to_string();
		assert!(!text.contains("hunter2"), "{text}");
		assert!(!text.contains("robot"), "{text}");
	}
	assert_eq!(created["has_registry_auth"], true);
	assert_eq!(listed[0]["has_registry_auth"], true);
	assert!(
		created.get("registry_password").is_none() && created.get("registry_auth").is_none(),
		"{created}"
	);

	// Encrypted at rest; decrypts to what was sent.
	let stored = s.stored(id).await;
	let sealed = stored.registry_auth.clone().unwrap();
	assert!(
		!String::from_utf8_lossy(&sealed).contains("hunter2"),
		"stored in the clear"
	);
	let plain: Value = serde_json::from_str(&s.state.secrets.decrypt(&sealed).unwrap()).unwrap();
	assert_eq!(
		plain,
		json!({ "username": "robot", "password": "hunter2-very-secret" })
	);

	// Saving without credentials keeps them; sending new ones replaces them.
	let (status, kept) = s
		.update(id, "private", json!({ "image": "ghcr.io/acme/app:2" }))
		.await;
	assert_eq!(status, StatusCode::OK, "{kept}");
	assert_eq!(kept["has_registry_auth"], true);
	assert_eq!(s.stored(id).await.registry_auth, Some(sealed.clone()));
	let (status, _) = s
		.update(
			id,
			"private",
			json!({ "image": "ghcr.io/acme/app:2", "registry_username": "robot2",
				"registry_password": "pw2" }),
		)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert_ne!(s.stored(id).await.registry_auth, Some(sealed));

	// Public images carry none.
	let (_, public) = s.create("public", json!({ "image": "alpine:3" })).await;
	assert_eq!(public["has_registry_auth"], false);
}

#[sqlx::test]
async fn image_app_deploy_pulls_and_sends_the_image_source(db: PgPool) {
	let state = state(db.clone());
	let seen = fake_agent(&state);
	let s = Setup::with_state(db, state).await;
	let (_, created) = s
		.create(
			"private",
			json!({ "image": "ghcr.io/acme/app:1", "registry_username": "robot",
				"registry_password": "pw" }),
		)
		.await;
	let id = created["id"].as_i64().unwrap();

	// The built spec: a fresh deploy pulls the tag again, with credentials.
	let app = s.stored(id).await;
	let fresh = deployments::insert(&s.db, app.id, None, None, None)
		.await
		.unwrap();
	let source = deployments::run::app_source(&s.state, &app, &fresh).unwrap_or_else(|_| panic!());
	let Source::Image {
		reference,
		pull,
		auth,
	} = source
	else {
		panic!("an image app deploys an image source");
	};
	assert_eq!(reference, "ghcr.io/acme/app:1");
	assert!(pull, "a deploy pulls the tag again");
	let auth = auth.expect("registry credentials reach the agent");
	assert_eq!(
		(auth.username.as_str(), auth.password.as_str()),
		("robot", "pw")
	);

	// And what actually goes over the wire.
	let first = s.deploy(id, None).await;
	let sent = seen.lock().unwrap()[0].clone();
	assert_eq!(sent["source"]["type"], "image");
	assert_eq!(sent["source"]["reference"], "ghcr.io/acme/app:1");
	assert_eq!(sent["source"]["pull"], true);
	assert_eq!(sent["source"]["auth"]["username"], "robot");
	assert_eq!(sent["service"], format!("app-{id}"));
	// The agent reports the digest, so that is what a rollback has.
	assert_eq!(first.image_tag.as_deref(), Some(DIGEST));
	assert_eq!(first.commit_sha, None);

	// Redeploying pulls again; restarting or rolling back reuses the digest
	// as it is on the server.
	s.deploy(id, None).await;
	let sent = seen.lock().unwrap()[1].clone();
	assert_eq!(sent["source"]["pull"], true);
	assert_eq!(sent["source"]["reference"], "ghcr.io/acme/app:1");
	s.deploy(id, Some(&first)).await;
	let sent = seen.lock().unwrap()[2].clone();
	assert_eq!(sent["source"]["reference"], DIGEST);
	assert_eq!(sent["source"]["pull"], false);
	assert!(sent["source"].get("auth").is_none(), "{sent}");
}

#[sqlx::test]
async fn image_app_git_deploys_still_build_from_the_repo(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, created) = s
		.create(
			"site",
			json!({ "repo_url": "https://example.com/r.git", "git_ref": "dev" }),
		)
		.await;
	let app = s.stored(created["id"].as_i64().unwrap()).await;
	let d = deployments::insert(&s.db, app.id, None, None, None)
		.await
		.unwrap();
	let source = deployments::run::app_source(&s.state, &app, &d).unwrap_or_else(|_| panic!());
	let Source::Git {
		repo_url, git_ref, ..
	} = source
	else {
		panic!("a git app builds from git");
	};
	assert_eq!(
		(repo_url.as_str(), git_ref.as_str()),
		("https://example.com/r.git", "dev")
	);
}

#[sqlx::test]
async fn image_app_webhook_deploys_pull_the_image_again(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, created) = s.create("hooked", json!({ "image": "alpine:3" })).await;
	let id = created["id"].as_i64().unwrap();
	let secret = created["webhook_secret"].as_str().unwrap().to_owned();

	let post = |secret: &str| {
		let req = Request::builder()
			.method("POST")
			.uri(format!("/webhooks/deploy/{id}"))
			.header("x-webhook-secret", secret)
			.header(header::CONTENT_TYPE, "application/json")
			// What Docker Hub sends when a tag is pushed.
			.body(Body::from(
				json!({ "push_data": { "tag": "3" }, "repository": { "repo_name": "alpine" } })
					.to_string(),
			))
			.unwrap();
		let app = s.app.clone();
		async move { app.oneshot(req).await.unwrap().status() }
	};
	assert_eq!(post("wrong").await, StatusCode::FORBIDDEN);
	assert_eq!(crate::tests::count(&s.db, "deployments").await, 0);
	assert_eq!(post(&secret).await, StatusCode::ACCEPTED);
	assert_eq!(post(&secret).await, StatusCode::ACCEPTED);

	let queued = deployments::list(&s.db, ApplicationId(id), None, 10)
		.await
		.unwrap();
	assert_eq!(queued.len(), 2);
	for d in &queued {
		// No image yet: the job pulls the reference, it does not reuse one.
		assert_eq!(d.image_tag, None);
		let app = s.stored(id).await;
		let source = deployments::run::app_source(&s.state, &app, d).unwrap_or_else(|_| panic!());
		assert!(matches!(source, Source::Image { pull: true, .. }));
	}
}

/// Deploys `docker.io/traefik/whoami` as an image app through the real
/// agent, in this process, and fetches it through Traefik.
///
/// `cargo test -p server --all-features -- --ignored image_app_live`
#[sqlx::test]
#[ignore = "pulls traefik/whoami and traefik from docker.io with a real podman.sock, binds ports 18080 and 18081"]
async fn image_app_live_whoami_answers_through_ingress(db: PgPool) {
	let mut state = state(db.clone());
	{
		let config = Arc::make_mut(&mut state.config);
		config.ingress_port = 18080;
		config.https_port = 18081;
	}
	let podman = agent::podman::Podman::connect(&agent::podman::local_socket_path())
		.expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let s = Setup::with_state(db, state).await;
	let (status, created) = s
		.create(
			"whoami-live",
			json!({ "image": "docker.io/traefik/whoami" }),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{created}");
	let id = created["id"].as_i64().unwrap();

	let outcome = std::panic::AssertUnwindSafe(async {
		let deployed = s.deploy(id, None).await;
		assert!(
			deployed
				.image_tag
				.as_deref()
				.is_some_and(|tag| tag.contains("@sha256:")),
			"the deployment records the digest: {:?}",
			deployed.image_tag
		);
		let host = created["url"].as_str().unwrap();
		let client = reqwest::Client::new();
		let mut last = None;
		for _ in 0..30 {
			tokio::time::sleep(std::time::Duration::from_millis(500)).await;
			match client
				.get("http://127.0.0.1:18080/")
				.header("Host", host)
				.send()
				.await
			{
				Ok(resp) if resp.status() == StatusCode::OK => return,
				other => last = Some(format!("{other:?}")),
			}
		}
		panic!("{host} never answered 200 through ingress: {last:?}");
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	// Leave nothing behind, whatever happened.
	for container in podman
		.service_containers(&format!("app-{id}"))
		.await
		.unwrap_or_default()
	{
		if let Some(name) = agent::podman::summary_name(&container) {
			let _ = podman.remove(name).await;
		}
	}
	let _ = podman.remove(agent::ingress::TRAEFIK_CONTAINER_NAME).await;
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}

/// The `podman` binary: on `PATH`, or where NixOS puts it.
fn podman_bin() -> &'static str {
	["podman", "/run/current-system/sw/bin/podman"]
		.into_iter()
		.find(|b| {
			std::process::Command::new(b)
				.arg("--version")
				.output()
				.is_ok()
		})
		.expect("a podman binary")
}

/// Runs the `podman` CLI; the live test below uses it to start and fill a registry.
fn podman_cli(args: &[&str]) -> std::process::Output {
	std::process::Command::new(podman_bin())
		.args(args)
		.output()
		.unwrap()
}

/// An image app pulls from a local `registry:2` that demands a login, using a saved
/// registry. The image is pushed there first and removed locally, so the only way to get
/// it is an authenticated pull.
///
/// `cargo test -p server --all-features -- --ignored image_app_live_saved_registry`
#[sqlx::test]
#[ignore = "starts registry:2 on 127.0.0.1:15000, pulls traefik/whoami, traefik and registry:2 with a real podman.sock, binds ports 28080 and 28443"]
async fn image_app_live_saved_registry_pulls_from_a_registry_that_needs_a_login(db: PgPool) {
	const REGISTRY: &str = "127.0.0.1:15000";
	const NAME: &str = "pk-k2-live-registry";
	let image = format!("{REGISTRY}/whoami:1");
	let dir = tempfile::tempdir().unwrap();
	// `robot` / `pk-registry-pass`, bcrypt, made with `htpasswd -Bbn`.
	let htpasswd = dir.path().join("htpasswd");
	std::fs::write(
		&htpasswd,
		"robot:$2y$05$IpccLqNPVtpEcaIPWeuduOnGeZl7Lra24Ei0if7papqttUMdzHkdK\n",
	)
	.unwrap();
	let authfile = dir.path().join("auth.json");
	let sock_dir = dir.path().join("sock");
	std::fs::create_dir_all(&sock_dir).unwrap();
	let socket = sock_dir.join("podman.sock");
	let mut service: Option<std::process::Child> = None;
	let cleanup = |service: &mut Option<std::process::Child>| {
		podman_cli(&["container", "rm", "-f", NAME]);
		podman_cli(&["image", "rm", "-f", &image]);
		if let Some(child) = service.as_mut() {
			let _ = child.kill();
			let _ = child.wait();
		}
	};
	cleanup(&mut service);

	// The registry speaks plain HTTP, so the podman that pulls from it is told it is
	// insecure. That is a private service on its own socket with its own registries.conf, so
	// the user's podman configuration is never touched.
	let conf = dir.path().join("registries.conf");
	std::fs::write(
		&conf,
		format!("[[registry]]\nlocation = \"{REGISTRY}\"\ninsecure = true\n"),
	)
	.unwrap();
	service = Some(
		std::process::Command::new(podman_bin())
			.env("CONTAINERS_REGISTRIES_CONF", &conf)
			.args(["system", "service", "--time=0"])
			.arg(format!("unix://{}", socket.display()))
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.spawn()
			.expect("start a private podman service"),
	);
	for _ in 0..50 {
		if socket.exists() {
			break;
		}
		tokio::time::sleep(std::time::Duration::from_millis(100)).await;
	}

	let mount = format!("{}:/auth/htpasswd:ro,Z", htpasswd.display());
	let started = podman_cli(&[
		"run",
		"-d",
		"--name",
		NAME,
		"-p",
		"127.0.0.1:15000:5000",
		"-v",
		&mount,
		"-e",
		"REGISTRY_AUTH=htpasswd",
		"-e",
		"REGISTRY_AUTH_HTPASSWD_REALM=podkit",
		"-e",
		"REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd",
		"docker.io/library/registry:2",
	]);
	assert!(started.status.success(), "{started:?}");

	let mut state = state(db.clone());
	{
		let config = Arc::make_mut(&mut state.config);
		config.ingress_port = 28080;
		config.https_port = 28443;
	}
	let socket_str = socket.to_str().unwrap().to_owned();
	let podman =
		agent::podman::Podman::connect(&socket_str).expect("connect to the private service");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: socket_str.clone(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});
	let s = Setup::with_state(db, state).await;

	let outcome = std::panic::AssertUnwindSafe(async {
		// Fill the registry, with the login, then forget the image locally.
		tokio::time::sleep(std::time::Duration::from_secs(2)).await;
		let auth = authfile.to_str().unwrap();
		for args in [
			vec!["pull", "docker.io/traefik/whoami"],
			vec!["tag", "docker.io/traefik/whoami", &image],
			vec![
				"login",
				"--tls-verify=false",
				"--authfile",
				auth,
				"-u",
				"robot",
				"-p",
				"pk-registry-pass",
				REGISTRY,
			],
			vec!["push", "--tls-verify=false", "--authfile", auth, &image],
			vec!["image", "rm", &image],
		] {
			let out = podman_cli(&args);
			assert!(out.status.success(), "podman {args:?}: {out:?}");
		}

		// Save the registry, then deploy an image app that picks it.
		let (status, saved) = call(
			&s.app,
			"POST",
			&format!("/api/v1/teams/{}/registries", s.team),
			Some(&s.token),
			Some(
				json!({ "name": "local", "url": REGISTRY, "username": "robot",
				"password": "pk-registry-pass" }),
			),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{saved}");
		let (status, test) = call(
			&s.app,
			"POST",
			&format!("/api/v1/teams/{}/registries/{}/test", s.team, saved["id"]),
			Some(&s.token),
			None,
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{test}");
		assert_eq!(
			test["ok"], true,
			"the registry accepts the saved login: {test}"
		);

		let (status, created) = s
			.create(
				"whoami-registry",
				json!({ "image": image, "registry_id": saved["id"] }),
			)
			.await;
		assert_eq!(status, StatusCode::OK, "{created}");
		let id = created["id"].as_i64().unwrap();
		let deployed = s.deploy(id, None).await;
		assert!(
			deployed
				.image_tag
				.as_deref()
				.is_some_and(|tag| tag.contains("@sha256:")),
			"the pull with the saved login recorded a digest: {:?}",
			deployed.image_tag
		);
		id
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	if let Ok(id) = &result {
		for container in podman
			.service_containers(&format!("app-{id}"))
			.await
			.unwrap_or_default()
		{
			if let Some(name) = agent::podman::summary_name(&container) {
				let _ = podman.remove(name).await;
			}
		}
	}
	let _ = podman.remove(agent::ingress::TRAEFIK_CONTAINER_NAME).await;
	cleanup(&mut service);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
