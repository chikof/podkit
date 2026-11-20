//! The live canvas test: an app and a Postgres of one environment, wired through the same
//! `projects::graph::wire` path the canvas POST runs, on a real Podman.
//!
//! Same prerequisites as `networks_live` (see `live.rs`): the `podkit-agent` binary, a
//! private `PODKIT_LIVE_PODMAN_SOCKET`, and the nginx, postgres, traefik images. Run it with:
//!
//! ```text
//! PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
//!   cargo test -p server --all-features -- --ignored canvas_live
//! ```

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::live::{HTTPS_PORT, INGRESS_PORT, Live, agent_binary, podman};
use super::{FakeAgent, Setup};
use crate::ids::{ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::projects::graph::{NodeKey, wire};
use crate::projects::overview::ResourceKind;
use crate::tests::state;

/// Wires an app to a Postgres of the same environment on the canvas path and checks, in the
/// real containers, that:
///
/// 1. before the wire the app has no `DATABASE_URL`;
/// 2. the wire writes `DATABASE_URL` as an ordinary variable and redeploys the app;
/// 3. the redeployed container has the variable, and the Postgres named in it answers the
///    Postgres protocol from inside the app container;
/// 4. unwiring removes the variable again;
/// 5. everything is removed.
#[sqlx::test]
#[ignore = "needs PODKIT_LIVE_PODMAN_SOCKET (a private podman.sock), the podkit-agent binary, nginx:alpine, postgres:18 and traefik images; binds ports 28080 and 28443"]
async fn canvas_live_wiring_an_app_to_postgres_injects_the_url_and_connects(db: PgPool) {
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
	let outcome = std::panic::AssertUnwindSafe(scenario(&s, &live));
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	// Leave nothing behind, whatever happened above.
	let listed = podman(&socket, &["ps", "-a", "--format", "{{.Names}}"])
		.await
		.unwrap_or_default();
	for name in listed.lines().filter(|n| n.starts_with("podkit-")) {
		let _ = podman(&socket, &["rm", "-f", "-t", "0", name]).await;
	}
	let nets = podman(&socket, &["network", "ls", "--format", "{{.Name}}"])
		.await
		.unwrap_or_default();
	for name in nets.lines().filter(|n| n.starts_with("podkit")) {
		let _ = podman(&socket, &["network", "rm", "-f", name]).await;
	}
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

/// The Postgres protocol's `SSLRequest`: a running server answers one byte, `S` or `N`.
const SSL_REQUEST: &str = r"printf '\000\000\000\010\004\322\026\057'";

async fn scenario(s: &Setup, live: &Live) {
	eprintln!("[canvas_live] creating an app and a Postgres in one environment");
	let (app, host) = s.image_app("shop", s.prod).await;
	let database = s.database_in(s.prod).await;
	s.deploy_for_real(app).await;
	let app_c = live.container(&format!("podkit-app-{app}-")).await;
	let db_alias = format!("podkit-db-{database}");
	live.container(&format!("podkit-db-{database}-")).await;

	eprintln!("[canvas_live] checking: before the wire the app has no DATABASE_URL");
	assert!(
		!live.succeeds(&app_c, "test -n \"$DATABASE_URL\"").await,
		"DATABASE_URL is set before anything was wired"
	);

	let user = {
		let id: i64 = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
			.fetch_one(&s.db)
			.await
			.unwrap();
		UserId(id)
	};
	let (team, project) = (TeamId(s.team), ProjectId(s.project));
	let from = NodeKey::new(ResourceKind::Application, app);
	let to = NodeKey::new(ResourceKind::Database, database);

	eprintln!("[canvas_live] wiring app -> database through projects::graph::wire");
	let applied = wire::wire(&s.state, user, team, project, from, to)
		.await
		.unwrap_or_else(|e| panic!("wire failed: {e:?}"));
	assert_eq!(applied.ends, (from, to));
	assert_eq!(applied.redeployed.len(), 1, "{applied:?}");

	// The variable is a normal env var, listed by the env var service.
	let vars =
		crate::env_vars::service::list(&s.db, user, team, project, crate::ids::ApplicationId(app))
			.await
			.unwrap();
	assert!(
		vars.iter().any(|v| v.key == "DATABASE_URL"),
		"no DATABASE_URL among {:?}",
		vars.iter().map(|v| &v.key).collect::<Vec<_>>()
	);

	eprintln!("[canvas_live] running the redeploy the wire queued");
	let deployment: i64 = sqlx::query_scalar(
		"SELECT id FROM deployments WHERE application_id = $1 ORDER BY id DESC LIMIT 1",
	)
	.bind(app)
	.fetch_one(&s.db)
	.await
	.unwrap();
	let job = jobs::latest_for(&s.db, JobKind::Deploy, deployment)
		.await
		.unwrap()
		.expect("the wire queued a deploy job");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	crate::deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap_or_else(|e| panic!("the redeploy failed: {e:?}"));
	log.finish().await;
	let app_c = live.container(&format!("podkit-app-{app}-")).await;

	eprintln!("[canvas_live] checking: the container has DATABASE_URL naming the Postgres");
	live.eventually(
		&app_c,
		&format!("printenv DATABASE_URL | grep -q '@{db_alias}:5432/'"),
		"the app has DATABASE_URL pointing at its Postgres",
	)
	.await;

	eprintln!("[canvas_live] checking: the app speaks the Postgres protocol to it");
	live.eventually(
		&app_c,
		&format!("{SSL_REQUEST} | nc -w3 {db_alias} 5432 | head -c1 | grep -q '[SN]'"),
		"the app gets a Postgres answer",
	)
	.await;

	eprintln!("[canvas_live] checking: the app still answers through Traefik");
	s.answers_through_ingress(&host).await;

	eprintln!("[canvas_live] unwiring: the variable goes");
	let graph = crate::projects::graph::build(&s.state, user, team, project)
		.await
		.unwrap();
	let edge = graph
		.edges
		.iter()
		.find(|e| e.from == from && e.to == to)
		.expect("the canvas shows the new edge")
		.clone();
	wire::unwire(&s.state, user, team, project, &edge)
		.await
		.unwrap_or_else(|e| panic!("unwire failed: {e:?}"));
	let vars =
		crate::env_vars::service::list(&s.db, user, team, project, crate::ids::ApplicationId(app))
			.await
			.unwrap();
	assert!(
		!vars.iter().any(|v| v.key == "DATABASE_URL"),
		"DATABASE_URL survived the unwire"
	);
	eprintln!("[canvas_live] all checks held");
}
