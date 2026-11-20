//! The live Gitea test: a real Gitea in a throwaway Podman, a real clone, a real build, a real
//! push webhook. Ignored by default; it pulls images and binds local ports.
//!
//! `PODMAN=/path/to/podman cargo test -p server --all-features -- --ignored gitea_live`
//!
//! It runs its own private `podman system service` (its own image store under `/tmp/pk-k3`),
//! so it never touches the containers or images of a running podkit, and removes the service
//! and the whole store when it ends, however it ends.

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::deployments::{self, Deployment, DeploymentStatus};
use crate::ids::ApplicationId;
use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::tests::{call, state, user_with_team};

const GITEA_PORT: u16 = 23000;
const INGRESS_PORT: u16 = 28080;
const HTTPS_PORT: u16 = 28081;
const GITEA_IMAGE: &str = "docker.io/gitea/gitea:1.24-rootless";
const DOCKERFILE: &str =
	"FROM docker.io/library/alpine:3.21\nCMD [\"sh\", \"-c\", \"while true; do sleep 60; done\"]\n";

fn podman_bin() -> String {
	std::env::var("PODMAN").unwrap_or_else(|_| "podman".to_owned())
}

/// A private Podman service and its store, removed when this is dropped.
struct Private {
	root: PathBuf,
	service: Child,
}

impl Private {
	fn start() -> Self {
		let root = PathBuf::from("/tmp/pk-k3").join(format!("live-{}", std::process::id()));
		std::fs::create_dir_all(&root).unwrap();
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
		Self { root, service }
	}

	fn socket(&self) -> PathBuf {
		self.root.join("podman.sock")
	}

	fn podman(&self, args: &[&str]) -> Output {
		Command::new(podman_bin())
			.arg("--url")
			.arg(format!("unix://{}", self.socket().display()))
			.args(args)
			.output()
			.expect("podman")
	}
}

impl Drop for Private {
	fn drop(&mut self) {
		let _ = self.podman(&["rm", "-f", "-a"]);
		let _ = self.service.kill();
		let _ = self.service.wait();
		// The store holds files owned by subuids: only `podman unshare` may remove them.
		let _ = Command::new(podman_bin())
			.args(["unshare", "rm", "-rf"])
			.arg(&self.root)
			.status();
		let _ = std::fs::remove_dir_all(&self.root);
	}
}

fn text(out: &Output) -> String {
	format!(
		"{}{}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	)
}

/// Waits for `check` to hold, up to `secs` seconds.
async fn wait_for<F, Fut>(what: &str, secs: u64, mut check: F)
where
	F: FnMut() -> Fut,
	Fut: std::future::Future<Output = bool>,
{
	for _ in 0..secs {
		if check().await {
			return;
		}
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	panic!("timed out waiting for {what}");
}

struct Gitea {
	http: reqwest::Client,
	token: String,
}

impl Gitea {
	fn url(path: &str) -> String {
		format!("http://127.0.0.1:{GITEA_PORT}/api/v1{path}")
	}

	async fn send(
		&self,
		method: reqwest::Method,
		path: &str,
		body: Option<Value>,
	) -> (StatusCode, Value) {
		let mut req = self
			.http
			.request(method, Self::url(path))
			.header("authorization", format!("token {}", self.token));
		if let Some(body) = body {
			req = req
				.header("content-type", "application/json")
				.body(body.to_string());
		}
		let res = req.send().await.expect("gitea answers");
		let status = StatusCode::from_u16(res.status().as_u16()).unwrap();
		let bytes = res.bytes().await.unwrap_or_default();
		(
			status,
			serde_json::from_slice(&bytes).unwrap_or(Value::Null),
		)
	}

	async fn add_file(&self, name: &str, content: &str, message: &str) {
		let (s, body) = self
			.send(
				reqwest::Method::POST,
				&format!("/repos/ops/demo/contents/{name}"),
				Some(json!({
					"content": base64::engine::general_purpose::STANDARD.encode(content),
					"message": message, "branch": "main",
				})),
			)
			.await;
		assert_eq!(s, StatusCode::CREATED, "add {name}: {body}");
	}

	async fn head(&self) -> String {
		let (_, branch) = self
			.send(reqwest::Method::GET, "/repos/ops/demo/branches/main", None)
			.await;
		branch["commit"]["id"].as_str().unwrap().to_owned()
	}
}

/// Queues a deploy job the way a webhook does and runs it the way the worker does.
async fn run_job(st: &AppState, deployment: &Deployment) -> Deployment {
	let job = jobs::latest_for(&st.db, JobKind::Deploy, deployment.id.0)
		.await
		.unwrap()
		.expect("a deploy job is queued");
	let mut log = st.logs.open(job.id).await.unwrap();
	let result = deployments::run::run(st, &job, &CancellationToken::new(), &mut log).await;
	log.finish().await;
	let after = deployments::find_by_id(&st.db, deployment.id)
		.await
		.unwrap()
		.unwrap();
	if let Err(e) = result {
		let text = std::fs::read_to_string(st.logs.path(job.id)).unwrap_or_default();
		panic!("deploy failed: {e:?}\n{after:?}\n{text}");
	}
	after
}

/// Connects a real Gitea, deploys an application from it, pushes a commit, and sees the push
/// webhook redeploy.
///
/// `podman` has to be on the PATH, or `PODMAN` has to name it.
///
/// `PODMAN=/run/current-system/sw/bin/podman cargo test -p server --all-features -- --ignored gitea_live`
#[sqlx::test]
#[ignore = "pulls gitea/gitea, traefik and alpine into a private Podman store; binds 23000, 28080 and 28081"]
async fn gitea_live_deploys_from_a_provider_and_a_push_redeploys(db: PgPool) {
	let private = Private::start();
	wait_for("the private podman service", 30, || async {
		private.socket().exists() && private.podman(&["info"]).status.success()
	})
	.await;

	// podkit, listening where the containers can reach it.
	let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
	let podkit_port = listener.local_addr().unwrap().port();
	let mut st = state(db.clone());
	{
		let config = Arc::make_mut(&mut st.config);
		config.ingress_port = INGRESS_PORT;
		config.https_port = HTTPS_PORT;
		config.public_url = Some(format!("http://host.containers.internal:{podkit_port}"));
	}
	let app = crate::routes::routes(st.clone()).await.unwrap();
	tokio::spawn({
		let app = app.clone();
		async move { axum::serve(listener, app).await.unwrap() }
	});

	// The agent, in this process, on the private Podman.
	let podman = agent::podman::Podman::connect(private.socket().to_str().unwrap())
		.expect("connect to the private podman");
	let data_dir = tempfile::tempdir().unwrap().keep();
	for sub in ["work", "backups", "traefik-acme", "gateway"] {
		std::fs::create_dir_all(data_dir.join(sub)).unwrap();
	}
	let agent_dir = data_dir.clone();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: private.socket().display().to_string(),
		http: reqwest::Client::new(),
	});
	let agent_listener = tokio::net::UnixListener::bind(&st.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(agent_listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	// Gitea, with an admin and a token.
	let started = private.podman(&[
		"run",
		"-d",
		"--name",
		"gitea",
		"-p",
		&format!("127.0.0.1:{GITEA_PORT}:3000"),
		"-e",
		"GITEA__security__INSTALL_LOCK=true",
		"-e",
		"GITEA__database__DB_TYPE=sqlite3",
		"-e",
		&format!("GITEA__server__ROOT_URL=http://127.0.0.1:{GITEA_PORT}/"),
		"-e",
		"GITEA__server__DISABLE_SSH=true",
		"-e",
		"GITEA__service__DISABLE_REGISTRATION=true",
		"-e",
		"GITEA__webhook__ALLOWED_HOST_LIST=*",
		GITEA_IMAGE,
	]);
	assert!(started.status.success(), "{}", text(&started));
	let http = reqwest::Client::new();
	wait_for("gitea", 180, || async {
		http.get(format!("http://127.0.0.1:{GITEA_PORT}/api/healthz"))
			.send()
			.await
			.is_ok_and(|r| r.status().is_success())
	})
	.await;
	let created = private.podman(&[
		"exec",
		"gitea",
		"gitea",
		"admin",
		"user",
		"create",
		"--username",
		"ops",
		"--password",
		"correct-horse-battery",
		"--email",
		"ops@example.com",
		"--admin",
		"--must-change-password=false",
	]);
	assert!(created.status.success(), "{}", text(&created));
	let minted = private.podman(&[
		"exec",
		"gitea",
		"gitea",
		"admin",
		"user",
		"generate-access-token",
		"--username",
		"ops",
		"--token-name",
		"podkit",
		"--scopes",
		"all",
		"--raw",
	]);
	assert!(minted.status.success(), "{}", text(&minted));
	let token = String::from_utf8_lossy(&minted.stdout).trim().to_owned();
	let token = token.split_whitespace().last().unwrap().to_owned();
	let gitea = Gitea {
		http: http.clone(),
		token: token.clone(),
	};

	// A private repository with a Dockerfile.
	let (s, repo) = gitea
		.send(
			reqwest::Method::POST,
			"/user/repos",
			Some(
				json!({ "name": "demo", "private": true, "auto_init": true, "default_branch": "main" }),
			),
		)
		.await;
	assert_eq!(s, StatusCode::CREATED, "{repo}");
	gitea
		.add_file("Dockerfile", DOCKERFILE, "add a Dockerfile")
		.await;
	let first_head = gitea.head().await;

	// podkit: the provider, then an application from it.
	let (api_token, team) = user_with_team(&app, "alice").await;
	let t = Some(api_token.as_str());
	let (s, provider) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/git-providers"),
		t,
		Some(json!({
			"kind": "gitea", "name": "live", "base_url": format!("http://127.0.0.1:{GITEA_PORT}"), "token": token,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{provider}");
	let (_, repos) = call(
		&app,
		"GET",
		&format!(
			"/api/v1/teams/{team}/git-providers/{}/repos?q=demo",
			provider["id"]
		),
		t,
		None,
	)
	.await;
	assert_eq!(repos["repos"][0]["full_name"], "ops/demo", "{repos}");
	let (_, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	let (_, servers) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		None,
	)
	.await;
	let (s, application) = call(
		&app,
		"POST",
		&format!(
			"/api/v1/teams/{team}/projects/{}/applications",
			project["id"]
		),
		t,
		Some(json!({
			"name": "demo", "slug": "demo", "server_id": servers[0]["id"],
			"git_provider_id": provider["id"], "git_repo": "ops/demo", "git_ref": "main",
			"container_port": 80,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{application}");
	assert_eq!(
		application["notice"],
		Value::Null,
		"the push webhook was made: {application}"
	);
	let id = ApplicationId(application["id"].as_i64().unwrap());

	// The webhook is at Gitea and points back at podkit.
	let (_, hooks) = gitea
		.send(reqwest::Method::GET, "/repos/ops/demo/hooks", None)
		.await;
	assert_eq!(hooks.as_array().unwrap().len(), 1, "{hooks}");
	assert_eq!(
		hooks[0]["config"]["url"],
		format!("http://host.containers.internal:{podkit_port}/webhooks/deploy/{id}")
	);

	let outcome = std::panic::AssertUnwindSafe(async {
		// First deploy: cloned with the provider's token, built, run.
		let stored = crate::applications::find_by_id(&db, id)
			.await
			.unwrap()
			.unwrap();
		let first = deployments::queue(&st, &stored, None, None).await.unwrap();
		let first = run_job(&st, &first).await;
		assert_eq!(first.status, DeploymentStatus::Running, "{first:?}");
		assert_eq!(first.commit_sha.as_deref(), Some(first_head.as_str()));

		// A push: Gitea calls the webhook, podkit queues a second deployment by itself.
		gitea
			.add_file("CHANGELOG.md", "# changes\n", "a second commit")
			.await;
		let second_head = gitea.head().await;
		assert_ne!(second_head, first_head);
		wait_for("the webhook to queue a deployment", 60, || async {
			deployments::list(&db, id, None, 10).await.unwrap().len() == 2
		})
		.await;
		let queued = deployments::list(&db, id, None, 10)
			.await
			.unwrap()
			.into_iter()
			.find(|d| d.id != first.id)
			.unwrap();
		let second = run_job(&st, &queued).await;
		assert_eq!(second.status, DeploymentStatus::Running, "{second:?}");
		assert_eq!(second.commit_sha.as_deref(), Some(second_head.as_str()));
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;
	drop(private);
	let _ = std::fs::remove_dir_all(&agent_dir);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
