//! The live shared-variables test: a team-level shared variable reaches a running
//! container's environment, checked with `podman exec` and `printenv`.
//!
//! Same prerequisites as `networks_live` (see `live.rs`): the `podkit-agent` binary, a
//! private `PODKIT_LIVE_PODMAN_SOCKET`, and the nginx and traefik images. Run it with:
//!
//! ```text
//! PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
//!   cargo test -p podkit-core --all-features -- --ignored shared_env_live
//! ```

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use super::live::{HTTPS_PORT, INGRESS_PORT, Live, agent_binary, podman};
use super::{FakeAgent, Setup};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::shared_env::{Scope, service};
use crate::tests::state;

/// Sets shared variables at team and project scope, gives the application two variables of its
/// own, deploys it for real and checks in the container that:
///
/// 1. a team-only variable is in the environment (`printenv`);
/// 2. the project's value beats the team's;
/// 3. the application's own value beats the shared one;
/// 4. a `${{shared.…}}` reference in an application value is replaced;
/// 5. everything is removed.
#[sqlx::test]
#[ignore = "needs PODKIT_LIVE_PODMAN_SOCKET (a private podman.sock), the podkit-agent binary, nginx:alpine and traefik images; binds ports 28080 and 28443"]
async fn shared_env_live_a_team_variable_reaches_the_running_container(db: PgPool) {
	let Some(socket) = crate::tests::live_podman_socket("shared_env_live") else {
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
	let _ = agent.kill().await;

	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}

async fn scenario(s: &Setup, live: &Live) {
	eprintln!("[shared_env_live] creating an application");
	let (app, host) = s.image_app("shop", s.prod).await;
	let user = {
		let id: i64 = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
			.fetch_one(&s.db)
			.await
			.unwrap();
		UserId(id)
	};
	let (team, project) = (TeamId(s.team), ProjectId(s.project));
	for (scope, key, value) in [
		(Scope::Team(team), "SHARED_LIVE_TEAM", "from-team"),
		(Scope::Team(team), "SHARED_LIVE_BOTH", "from-team"),
		(
			Scope::Project(team, project),
			"SHARED_LIVE_BOTH",
			"from-project",
		),
		(Scope::Team(team), "SHARED_LIVE_OWN", "from-team"),
		(Scope::Team(team), "SHARED_LIVE_HOST", "db.internal"),
	] {
		service::set(&s.state, user, scope, key, value, true)
			.await
			.unwrap();
	}
	for (key, value) in [
		("SHARED_LIVE_OWN", "from-app"),
		(
			"SHARED_LIVE_URL",
			"postgres://${{shared.SHARED_LIVE_HOST}}/shop",
		),
	] {
		crate::env_vars::service::upsert(
			&s.state,
			user,
			team,
			project,
			ApplicationId(app),
			key,
			value,
		)
		.await
		.unwrap();
	}

	s.deploy_for_real(app).await;
	let container = live.container(&format!("podkit-app-{app}-")).await;
	for (name, want) in [
		("SHARED_LIVE_TEAM", "from-team"),
		("SHARED_LIVE_BOTH", "from-project"),
		("SHARED_LIVE_OWN", "from-app"),
		("SHARED_LIVE_URL", "postgres://db.internal/shop"),
	] {
		eprintln!("[shared_env_live] checking: printenv {name} = {want}");
		live.eventually(
			&container,
			&format!("test \"$(printenv {name})\" = '{want}'"),
			name,
		)
		.await;
	}
	s.answers_through_ingress(&host).await;
	eprintln!("[shared_env_live] all checks held");
}
