//! The live network test: real agent, real Podman, real Traefik.
//!
//! It needs the `podkit-agent` binary built next to the test binary
//! (`cargo build -p agent`) and a Podman API socket of its own, named by
//! `PODKIT_LIVE_PODMAN_SOCKET`. It refuses to run without one so it can never
//! touch the Podman of the person running it. Run it with:
//!
//! ```text
//! PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
//!   cargo test -p server --all-features -- --ignored networks_live
//! ```

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::{FakeAgent, Setup};
use crate::applications;
use crate::ids::ApplicationId;
use crate::jobs::{self, JobKind};
use crate::tests::state;

const INGRESS_PORT: u16 = 28080;
const HTTPS_PORT: u16 = 28443;
const APP_IMAGE: &str = "docker.io/library/nginx:alpine";

/// `podman` against the private socket. `Err` carries the output.
async fn podman(socket: &str, args: &[&str]) -> Result<String, String> {
	let program = ["/run/current-system/sw/bin/podman", "/usr/bin/podman"]
		.into_iter()
		.find(|p| std::path::Path::new(p).exists())
		.unwrap_or("podman");
	let out = tokio::process::Command::new(program)
		.arg("--url")
		.arg(format!("unix://{socket}"))
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

struct Live {
	socket: String,
}

impl Live {
	/// The name of the one container whose name starts with `prefix`.
	async fn container(&self, prefix: &str) -> String {
		let out = podman(
			&self.socket,
			&[
				"ps",
				"--filter",
				&format!("name={prefix}"),
				"--format",
				"{{.Names}}",
			],
		)
		.await
		.unwrap();
		let names: Vec<&str> = out.lines().filter(|n| n.starts_with(prefix)).collect();
		assert_eq!(names.len(), 1, "one container named {prefix}*: {out}");
		names[0].to_owned()
	}

	/// Runs `script` in `container`; `true` when it exits 0.
	async fn succeeds(&self, container: &str, script: &str) -> bool {
		podman(&self.socket, &["exec", container, "sh", "-c", script])
			.await
			.is_ok()
	}

	/// Polls until `script` succeeds in `container`.
	async fn eventually(&self, container: &str, script: &str, what: &str) {
		let start = Instant::now();
		while start.elapsed() < Duration::from_secs(90) {
			if self.succeeds(container, script).await {
				return;
			}
			tokio::time::sleep(Duration::from_millis(700)).await;
		}
		let out = podman(&self.socket, &["exec", container, "sh", "-c", script]).await;
		panic!("{what}: never held in {container} ({script}): {out:?}");
	}

	async fn ip_of(&self, container: &str, network: &str) -> String {
		podman(
			&self.socket,
			&[
				"inspect",
				"-f",
				&format!("{{{{(index .NetworkSettings.Networks \"{network}\").IPAddress}}}}"),
				container,
			],
		)
		.await
		.unwrap()
		.trim()
		.to_owned()
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

impl Setup {
	async fn image_app(&self, slug: &str, environment: i64) -> (i64, String) {
		let (status, app) = self
			.send(
				"POST",
				&format!(
					"/api/v1/teams/{}/projects/{}/applications",
					self.team, self.project
				),
				Some(json!({
					"name": slug, "slug": slug, "server_id": self.server,
					"environment_id": environment, "image": APP_IMAGE,
					"container_port": 80,
				})),
			)
			.await;
		assert_eq!(status, StatusCode::OK, "{app}");
		(
			app["id"].as_i64().unwrap(),
			app["url"].as_str().unwrap().to_owned(),
		)
	}

	async fn stored(&self, id: i64) -> applications::Application {
		applications::find_by_id(&self.db, ApplicationId(id))
			.await
			.unwrap()
			.unwrap()
	}

	/// Deploys the way the API and the worker do.
	async fn deploy_for_real(&self, id: i64) {
		let app = self.stored(id).await;
		let queued = crate::deployments::queue(&self.state, &app, None, None)
			.await
			.unwrap();
		let job = jobs::latest_for(&self.db, JobKind::Deploy, queued.id.0)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		crate::deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log)
			.await
			.unwrap_or_else(|e| panic!("deploy of app {id} failed: {e:?}"));
		log.finish().await;
	}

	async fn database_in(&self, environment: i64) -> i64 {
		let (status, database) = self
			.send(
				"POST",
				&format!(
					"/api/v1/teams/{}/projects/{}/databases",
					self.team, self.project
				),
				Some(json!({
					"name": format!("db{environment}"), "engine": "postgres",
					"server_id": self.server, "environment_id": environment,
				})),
			)
			.await;
		assert_eq!(status, StatusCode::ACCEPTED, "{database}");
		let id = database["id"].as_i64().unwrap();
		let job = jobs::latest_for(&self.db, JobKind::DatabaseProvision, id)
			.await
			.unwrap()
			.expect("a provision job");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		crate::databases::run::run(&self.state, &job, &CancellationToken::new(), &mut log)
			.await
			.unwrap_or_else(|e| panic!("provisioning database {id} failed: {e:?}"));
		log.finish().await;
		id
	}

	async fn through_ingress(&self, host: &str) -> Option<StatusCode> {
		self.state
			.http
			.get(format!("http://127.0.0.1:{INGRESS_PORT}/"))
			.header("Host", host)
			.send()
			.await
			.ok()
			.map(|r| r.status())
	}

	async fn answers_through_ingress(&self, host: &str) {
		let mut last = None;
		for _ in 0..60 {
			last = self.through_ingress(host).await;
			if last == Some(StatusCode::OK) {
				return;
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
		let socket = std::env::var("PODKIT_LIVE_PODMAN_SOCKET").unwrap_or_default();
		let logs = podman(&socket, &["logs", "--tail", "15", "podkit-traefik"]).await;
		let nets = podman(
			&socket,
			&[
				"inspect",
				"podkit-traefik",
				"-f",
				"{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
			],
		)
		.await;
		panic!(
			"{host} never answered 200 through Traefik: {last:?}\nTraefik on {nets:?}\n{logs:?}"
		);
	}
}

/// Two environments with an app and a Postgres each, on a real Podman:
///
/// 1. an app of one environment cannot resolve or connect to the Postgres of
///    the other, but reaches its own;
/// 2. a user network that both apps are attached to lets them reach each
///    other by alias;
/// 3. both apps answer through Traefik, before and after;
/// 4. an app linked to a model reaches the gateway at `llm.podkit.internal`,
///    and an app of an environment without a link does not;
/// 5. the migrate job moves a container off the shared `podkit` network
///    without a gap;
/// 6. everything is removed.
#[sqlx::test]
#[ignore = "needs PODKIT_LIVE_PODMAN_SOCKET (a private podman.sock), the podkit-agent binary, nginx:alpine, postgres:18, traefik and python:3.13-slim images; binds ports 28080 and 28443"]
async fn networks_live_environments_are_isolated_and_user_networks_connect(db: PgPool) {
	let socket = std::env::var("PODKIT_LIVE_PODMAN_SOCKET")
		.expect("set PODKIT_LIVE_PODMAN_SOCKET to a private podman.sock; never the host's");
	let live = Live {
		socket: socket.clone(),
	};

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
		.arg(&socket)
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
	let ids = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
	let outcome = {
		let (live, ids) = (&live, ids.clone());
		std::panic::AssertUnwindSafe(scenario(&s, live, ids))
	};
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	// Leave nothing behind, whatever happened above.
	let names = ids.lock().unwrap().clone();
	for name in &names {
		let _ = podman(&socket, &["rm", "-f", "-t", "0", name]).await;
	}
	for name in ["podkit-traefik", "podkit-gateway", "podkit-ingress-refresh"] {
		let _ = podman(&socket, &["rm", "-f", "-t", "0", name]).await;
	}
	let mut networks: Vec<String> = sqlx::query_scalar("SELECT podman_name FROM networks")
		.fetch_all(&s.db)
		.await
		.unwrap_or_default();
	networks.push(format!("podkit-env-{}", s.prod));
	networks.push(format!("podkit-env-{}", s.staging));
	for name in networks {
		let _ = podman(&socket, &["network", "rm", "-f", &name]).await;
	}
	let _ = podman(&socket, &["network", "rm", "-f", "podkit"]).await;
	let volumes = podman(&socket, &["volume", "ls", "--format", "{{.Name}}"])
		.await
		.unwrap_or_default();
	for volume in volumes.lines().filter(|v| v.starts_with("podkit-db-")) {
		let _ = podman(&socket, &["volume", "rm", "-f", volume]).await;
	}
	let _ = agent.kill().await;

	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}

#[allow(clippy::too_many_lines)] // one linear scenario; splitting it hides the order
async fn scenario(s: &Setup, live: &Live, ids: Arc<std::sync::Mutex<Vec<String>>>) {
	// Whatever runs is removed afterwards, by name.
	let track = |name: String| ids.lock().unwrap().push(name);

	eprintln!("[networks_live] deploying two environments, each with an app and a Postgres");
	// 1. Two environments, each with an app and a Postgres.
	let ((prod_app, prod_host), (staging_app, staging_host)) = (
		s.image_app("shop", s.prod).await,
		s.image_app("shop-staging", s.staging).await,
	);
	let (prod_db, staging_db) = (s.database_in(s.prod).await, s.database_in(s.staging).await);
	s.deploy_for_real(prod_app).await;
	s.deploy_for_real(staging_app).await;
	let prod_app_c = live.container(&format!("podkit-app-{prod_app}-")).await;
	let staging_app_c = live.container(&format!("podkit-app-{staging_app}-")).await;
	let prod_db_c = live.container(&format!("podkit-db-{prod_db}-")).await;
	let staging_db_c = live.container(&format!("podkit-db-{staging_db}-")).await;
	for c in [&prod_app_c, &staging_app_c, &prod_db_c, &staging_db_c] {
		track(c.clone());
	}
	let (prod_net, staging_net) = (
		format!("podkit-env-{}", s.prod),
		format!("podkit-env-{}", s.staging),
	);

	eprintln!("[networks_live] checking: no resource sits on the shared podkit network");
	// Nothing sits on the shared network any more.
	let on_podkit = podman(
		&live.socket,
		&[
			"network",
			"inspect",
			"podkit",
			"-f",
			"{{range .Containers}}{{.Name}} {{end}}",
		],
	)
	.await
	.unwrap_or_default();
	for c in [&prod_app_c, &staging_app_c, &prod_db_c, &staging_db_c] {
		assert!(
			!on_podkit.contains(c.as_str()),
			"{c} is on `podkit`: {on_podkit}"
		);
	}

	eprintln!("[networks_live] checking: each app reaches its own Postgres");
	// Its own Postgres answers once it has initialised.
	let (prod_db_alias, staging_db_alias) = (
		format!("podkit-db-{prod_db}"),
		format!("podkit-db-{staging_db}"),
	);
	live.eventually(
		&prod_app_c,
		&format!("getent hosts {prod_db_alias} && nc -z -w2 {prod_db_alias} 5432"),
		"the production app reaches its own Postgres",
	)
	.await;
	live.eventually(
		&staging_app_c,
		&format!("nc -z -w2 {staging_db_alias} 5432"),
		"the staging app reaches its own Postgres",
	)
	.await;

	eprintln!("[networks_live] checking: prod cannot resolve or connect to staging Postgres");
	// 2. Production cannot resolve or connect to staging's Postgres.
	assert!(
		!live
			.succeeds(&prod_app_c, &format!("getent hosts {staging_db_alias}"))
			.await,
		"the production app resolved staging's Postgres"
	);
	let staging_db_ip = live.ip_of(&staging_db_c, &staging_net).await;
	assert!(!staging_db_ip.is_empty());
	assert!(
		!live
			.succeeds(&prod_app_c, &format!("nc -z -w2 {staging_db_ip} 5432"))
			.await,
		"the production app connected to staging's Postgres by address"
	);
	assert!(
		!live
			.succeeds(&staging_app_c, &format!("getent hosts {prod_db_alias}"))
			.await,
		"the staging app resolved production's Postgres"
	);

	eprintln!("[networks_live] checking: both apps answer through Traefik");
	// 4. Both answer through Traefik.
	s.answers_through_ingress(&prod_host).await;
	s.answers_through_ingress(&staging_host).await;

	eprintln!(
		"[networks_live] attaching a user network to both apps, redeploying, checking alias reach"
	);
	// 3. A user network that both apps are attached to.
	let net = s.create_network("Bridge", false).await;
	assert_eq!(s.attach(net, prod_app).await, StatusCode::NO_CONTENT);
	assert_eq!(s.attach(net, staging_app).await, StatusCode::NO_CONTENT);
	s.deploy_for_real(prod_app).await;
	s.deploy_for_real(staging_app).await;
	let prod_app_c = live.container(&format!("podkit-app-{prod_app}-")).await;
	let staging_app_c = live.container(&format!("podkit-app-{staging_app}-")).await;
	track(prod_app_c.clone());
	track(staging_app_c.clone());
	live.eventually(
		&prod_app_c,
		"wget -q -O /dev/null -T 3 http://shop-staging/",
		"the production app reaches the staging app by alias",
	)
	.await;
	live.eventually(
		&staging_app_c,
		"wget -q -O /dev/null -T 3 http://shop/",
		"the staging app reaches the production app by alias",
	)
	.await;
	assert!(
		!live
			.succeeds(&prod_app_c, &format!("getent hosts {staging_db_alias}"))
			.await,
		"the user network must not expose staging's Postgres"
	);

	eprintln!("[networks_live] checking: both apps still answer through Traefik");
	// 4. Still answering through Traefik after the redeploy.
	s.answers_through_ingress(&prod_host).await;
	s.answers_through_ingress(&staging_host).await;

	eprintln!("[networks_live] linking a model, checking llm.podkit.internal on prod only");
	// 5. A model linked to the production app: the gateway joins that
	// environment's network, and only that one.
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
				"/api/v1/teams/{}/projects/{}/applications/{prod_app}/models",
				s.team, s.project
			),
			Some(json!({ "model_id": model })),
		)
		.await;
	assert_eq!(status, StatusCode::CREATED, "{link}");
	let gateway = proto::GATEWAY_HOST;
	live.eventually(
		&prod_app_c,
		&format!(
			"getent hosts {gateway} && nc -z -w2 {gateway} {}",
			proto::GATEWAY_PORT
		),
		"the linked app reaches the gateway",
	)
	.await;
	assert!(
		!live
			.succeeds(&staging_app_c, &format!("getent hosts {gateway}"))
			.await,
		"an app without a link resolved the gateway"
	);
	let on = podman(
		&live.socket,
		&[
			"inspect",
			"podkit-gateway",
			"-f",
			"{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
		],
	)
	.await
	.unwrap();
	assert!(on.contains(&prod_net) && !on.contains(&staging_net), "{on}");

	eprintln!("[networks_live] running the migrate job: it moves a legacy container without a gap");
	// 6. The migrate job: a container put back on the shared network (as a
	// pre-0040 install has it) moves to its environment network and stays
	// reachable through Traefik the whole time. The legacy install is made
	// by hand: a Traefik started the way the old agent did (on `podkit`
	// only, `--providers.docker.network=podkit`), and an app container with no
	// `traefik.docker.network` label that sits on `podkit` alone.
	let socket_path = live.socket.clone();
	let _ = podman(&live.socket, &["rm", "-f", "-t", "0", "podkit-traefik"]).await;
	podman(
		&live.socket,
		&[
			"run",
			"-d",
			"--name",
			"podkit-traefik",
			"--network",
			"podkit",
			"--label",
			"podkit.config-hash=before-0040",
			"-p",
			&format!("127.0.0.1:{INGRESS_PORT}:80"),
			"-v",
			&format!("{socket_path}:/var/run/docker.sock"),
			"docker.io/library/traefik:v3.7.19",
			"--providers.docker=true",
			"--providers.docker.endpoint=unix:///var/run/docker.sock",
			"--providers.docker.exposedbydefault=false",
			"--providers.docker.network=podkit",
			"--entrypoints.web.address=:80",
		],
	)
	.await
	.expect("start a Traefik of the old kind");
	let legacy = format!("podkit-app-{prod_app}-900");
	track(legacy.clone());
	podman(
		&live.socket,
		&[
			"run",
			"-d",
			"--name",
			&legacy,
			"--network",
			"podkit",
			"--label",
			"traefik.enable=true",
			"--label",
			"traefik.http.routers.legacy.rule=Host(`legacy.live.test`)",
			"--label",
			"traefik.http.routers.legacy.entrypoints=web",
			"--label",
			"traefik.http.services.legacy.loadbalancer.server.port=80",
			APP_IMAGE,
		],
	)
	.await
	.expect("start a legacy app container");
	eprintln!("[networks_live] legacy answers through the old Traefik?");
	s.answers_through_ingress("legacy.live.test").await;
	let networks_of = |name: String| {
		let socket = live.socket.clone();
		async move {
			podman(
				&socket,
				&[
					"inspect",
					&name,
					"-f",
					"{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
				],
			)
			.await
			.unwrap()
		}
	};
	assert_eq!(networks_of("podkit-traefik".into()).await.trim(), "podkit");

	let user = {
		let id: i64 = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
			.fetch_one(&s.db)
			.await
			.unwrap();
		crate::ids::UserId(id)
	};
	let migrate = || {
		crate::networks::migrate::run(
			&s.state,
			user,
			crate::ids::TeamId(s.team),
			crate::ids::ServerId(s.server),
		)
	};

	// The old Traefik is replaced (ingress restarts for a moment); the
	// legacy container is moved in the same run.
	let first = migrate().await.unwrap();
	assert!(first.moved.contains(&legacy), "{first:?}");
	let hash = podman(
		&live.socket,
		&[
			"inspect",
			"podkit-traefik",
			"-f",
			"{{index .Config.Labels \"podkit.config-hash\"}}",
		],
	)
	.await
	.unwrap();
	assert_ne!(hash.trim(), "before-0040", "Traefik was not replaced");
	let on = networks_of(legacy.clone()).await;
	assert!(!on.split_whitespace().any(|n| n == "podkit"), "{on}");
	assert!(on.contains(&prod_net), "{on}");
	eprintln!("[networks_live] legacy and prod answer after the first migrate?");
	s.answers_through_ingress("legacy.live.test").await;
	s.answers_through_ingress(&prod_host).await;

	// The move itself has no gap: put the legacy container back on `podkit`
	// (the way it was) and watch it while the job moves it again.
	podman(&live.socket, &["network", "connect", "podkit", &legacy])
		.await
		.expect("rejoin podkit");
	eprintln!("[networks_live] legacy answers again after rejoining podkit?");
	s.answers_through_ingress("legacy.live.test").await;
	let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
	let watcher = {
		let (flag, client) = (stop.clone(), s.state.http.clone());
		tokio::spawn(async move {
			let (mut failures, mut total) = (0u32, 0u32);
			while !flag.load(std::sync::atomic::Ordering::Relaxed) {
				total += 1;
				let ok = client
					.get(format!("http://127.0.0.1:{INGRESS_PORT}/"))
					.header("Host", "legacy.live.test")
					.send()
					.await
					.is_ok_and(|r| r.status() == 200);
				if !ok {
					failures += 1;
				}
				tokio::time::sleep(Duration::from_millis(20)).await;
			}
			(failures, total)
		})
	};
	let second = migrate().await.unwrap();
	stop.store(true, std::sync::atomic::Ordering::Relaxed);
	let (failures, total) = watcher.await.unwrap();
	eprintln!(
		"[networks_live] moved {:?}; {failures} failed of {total} requests while it moved",
		second.moved
	);
	assert!(second.moved.contains(&legacy), "{second:?}");
	assert_eq!(failures, 0, "requests failed while the container moved");
	let on = networks_of(legacy).await;
	assert!(!on.split_whitespace().any(|n| n == "podkit"), "{on}");
	s.answers_through_ingress("legacy.live.test").await;
	eprintln!("[networks_live] all checks held");
}
