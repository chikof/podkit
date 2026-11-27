//! The live preset test: a preset applied with "deploy", on a real Podman.
//!
//! Same prerequisites as `networks_live` (see `live.rs`): the `podkit-agent` binary, a
//! private `PODKIT_LIVE_PODMAN_SOCKET`, and the nginx, postgres, traefik images. Run it with:
//!
//! ```text
//! PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
//!   cargo test -p podkit-core --all-features -- --ignored preset_live
//! ```

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::live::{HTTPS_PORT, INGRESS_PORT, Live, podman};
use super::{FakeAgent, Setup};
use crate::jobs::{self, JobKind};
use crate::tests::state;

/// A preset as a team would keep it: no server chosen, a slug made from the project's, one
/// secret podkit makes up.
const PRESET: &str = r#"version = 1

[projects.site]
name = "Site"

[projects.site.environments.production]
name = "Production"

[projects.site.environments.production.apps.web]
server = "${choose}"
slug = "${project}-web"
image = "docker.io/library/nginx:alpine"
port = 80

[projects.site.environments.production.apps.web.env]
SESSION_KEY = "${generate:session-key}"

[projects.site.environments.production.databases.main]
server = "${choose}"
engine = "postgres"
"#;

/// Applies a preset into a new project with "deploy" and checks, in the real containers,
/// that:
///
/// 1. the application runs, with the secret podkit made up for it;
/// 2. the Postgres runs and the application reaches it by name;
/// 3. the application answers through Traefik;
/// 4. everything is removed.
#[sqlx::test]
#[ignore = "needs PODKIT_LIVE_PODMAN_SOCKET (a private podman.sock), the podkit-agent binary, nginx:alpine, postgres:18 and traefik images; binds ports 28080/28443"]
async fn preset_live_an_applied_preset_runs_and_answers(db: PgPool) {
	let Some(socket) = crate::tests::live_podman_socket("preset_live") else {
		return;
	};
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
	let mut agent = tokio::process::Command::new(crate::tests::agent_binary())
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

async fn scenario(s: &Setup, live: &Live) {
	let server: String = sqlx::query_scalar("SELECT name FROM servers WHERE id = $1")
		.bind(s.server)
		.fetch_one(&s.db)
		.await
		.unwrap();

	eprintln!("[preset_live] applying the preset into a new project, with deploy");
	let (status, done) = s
		.send(
			"POST",
			&format!("/api/v1/teams/{}/presets/apply", s.team),
			Some(json!({
				"preset": PRESET,
				"answers": { "servers": {
					"production/app/web": server,
					"production/database/main": server,
				} },
				"target": { "new": { "slug": "live", "name": "Live" } },
				"deploy": true,
			})),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{done}");
	let project = done["done"]["project_id"].as_i64().unwrap();
	let app: i64 = sqlx::query_scalar("SELECT id FROM applications WHERE slug = 'live-web'")
		.fetch_one(&s.db)
		.await
		.unwrap_or_else(|e| panic!("no application named after the project: {e} {done}"));
	let database: i64 = sqlx::query_scalar("SELECT id FROM databases WHERE project_id = $1")
		.bind(project)
		.fetch_one(&s.db)
		.await
		.unwrap();

	eprintln!("[preset_live] running the jobs the apply queued");
	let job = jobs::latest_for(&s.db, JobKind::DatabaseProvision, database)
		.await
		.unwrap()
		.expect("the apply queued a provision job");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	crate::databases::run::run(&s.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap_or_else(|e| panic!("provisioning failed: {e:?}"));
	log.finish().await;
	let deployment: i64 = sqlx::query_scalar(
		"SELECT id FROM deployments WHERE application_id = $1 ORDER BY id DESC LIMIT 1",
	)
	.bind(app)
	.fetch_one(&s.db)
	.await
	.unwrap_or_else(|e| panic!("the apply queued no deployment: {e} {done}"));
	let job = jobs::latest_for(&s.db, JobKind::Deploy, deployment)
		.await
		.unwrap()
		.expect("the apply queued a deploy job");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	crate::deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap_or_else(|e| panic!("the deploy failed: {e:?}"));
	log.finish().await;

	eprintln!("[preset_live] checking: the application runs with its made-up secret");
	let app_c = live.container(&format!("podkit-app-{app}-")).await;
	live.eventually(
		&app_c,
		"printenv SESSION_KEY | grep -Eq '^[0-9a-f]{32}$'",
		"the app has the secret podkit made up",
	)
	.await;

	eprintln!("[preset_live] checking: the Postgres runs and the application reaches it");
	live.container(&format!("podkit-db-{database}-")).await;
	live.eventually(
		&app_c,
		&format!("nc -z -w2 podkit-db-{database} 5432"),
		"the app reaches the preset's Postgres",
	)
	.await;

	eprintln!("[preset_live] checking: the application answers through Traefik");
	let (_, apps) = s
		.send(
			"GET",
			&format!("/api/v1/teams/{}/projects/{project}/applications", s.team),
			None,
		)
		.await;
	let host = apps[0]["url"]
		.as_str()
		.unwrap_or_else(|| panic!("the application has no url: {apps}"))
		.to_owned();
	s.answers_through_ingress(&host).await;
	eprintln!("[preset_live] all checks held");
}
