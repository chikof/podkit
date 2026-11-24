//! The live test of scheduled tasks: the real agent, in this process, and a real Podman.
//!
//! It needs a Podman API socket of its own, named by `PODKIT_LIVE_PODMAN_SOCKET`, so it can
//! never touch the Podman of the person running it (and so it does not depend on how that
//! one is run: an exec through some system-wide services fails in `conmon`). Start one with
//! `podman --root R --runroot X system service --time=900 unix:///path/podman.sock`, then:
//!
//! ```text
//! PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
//!   cargo test -p podkit-core --all-features -- --ignored cron_live
//! ```
//!
//! Two tasks run every minute through the server's own dispatcher and scheduler:
//! a one-shot container (`busybox date`) that must run twice, and an exec task (`whoami`)
//! in a running busybox container. Their logs are read back from the job log files.
//! `docker.io/library/busybox` is pulled into that Podman when it is not there (a few MB).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use sqlx::PgPool;

use super::{CronRun, RunStatus, store};
use crate::ids::CronTaskId;
use crate::tests::{call, state, user_with_team};

const IMAGE: &str = "docker.io/library/busybox:latest";

/// The private socket, or a refusal to run.
fn socket() -> String {
	// Only reached once the test has found the variable set (see `live_podman_socket`).
	std::env::var("PODKIT_LIVE_PODMAN_SOCKET").unwrap_or_default()
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

/// `podman` against the private socket.
fn podman(args: &[&str]) -> std::process::Output {
	std::process::Command::new(podman_bin())
		.arg("--url")
		.arg(format!("unix://{}", socket()))
		.args(args)
		.output()
		.unwrap()
}

async fn runs_of(db: &PgPool, task: i64) -> Vec<CronRun> {
	store::list_runs(db, CronTaskId(task), None, 100)
		.await
		.unwrap()
}

#[sqlx::test]
#[ignore = "runs real containers in the Podman named by PODKIT_LIVE_PODMAN_SOCKET, pulls busybox, takes about three minutes"]
async fn cron_live_a_container_task_runs_twice_and_an_exec_task_runs_in_a_container(db: PgPool) {
	let state = state(db.clone());
	let Some(socket) = crate::tests::live_podman_socket("cron_live") else {
		return;
	};
	let podman_agent =
		agent::podman::Podman::connect(&socket).expect("connect to the private podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman_agent.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman_agent.clone(), &data_dir).await,
		data_dir,
		podman_socket: socket.clone(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let router = crate::routes::routes(state.clone()).await.unwrap();
	let (token, team) = user_with_team(&router, "alice").await;
	let (_, servers) = call(
		&router,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		Some(&token),
		None,
	)
	.await;
	let server = servers[0]["id"].as_i64().unwrap();
	let (_, project) = call(
		&router,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "live", "slug": "live" })),
	)
	.await;
	let project = project["id"].as_i64().unwrap();
	let (s, app) = call(
		&router,
		"POST",
		&format!("/api/v1/teams/{team}/projects/{project}/applications"),
		Some(&token),
		Some(json!({
			"name": "box", "slug": "box", "server_id": server,
			"image": IMAGE, "container_port": 80,
		})),
	)
	.await;
	assert!(s.is_success(), "{app}");
	let application = app["id"].as_i64().unwrap();
	let env: i64 = sqlx::query_scalar("SELECT environment_id FROM applications WHERE id = $1")
		.bind(application)
		.fetch_one(&db)
		.await
		.unwrap();

	// The container the exec task runs in: a plain busybox the test starts and removes.
	let name = format!("pk-cron-live-{}", std::process::id());
	let started = podman(&["run", "-d", "--name", &name, IMAGE, "sleep", "600"]);
	assert!(
		started.status.success(),
		"podman run: {}",
		String::from_utf8_lossy(&started.stderr)
	);
	sqlx::query(
		"INSERT INTO deployments (id, application_id, status, container_id, image_tag)
		VALUES ($1, $2, 'running', $3, $4)",
	)
	.bind(crypto::generate_id())
	.bind(application)
	.bind(&name)
	.bind(IMAGE)
	.execute(&db)
	.await
	.unwrap();

	let base = format!("/api/v1/teams/{team}/projects/{project}/cron");
	let (s, date_task) = call(
		&router,
		"POST",
		&base,
		Some(&token),
		Some(json!({
			"environment_id": env, "name": "date", "kind": "container",
			"server_id": server, "image": IMAGE, "command": "date -u",
			"schedule": "* * * * *", "timeout_s": 60,
		})),
	)
	.await;
	assert_eq!(s, axum::http::StatusCode::CREATED, "{date_task}");
	let date_task = date_task["id"].as_i64().unwrap();
	let (s, whoami_task) = call(
		&router,
		"POST",
		&base,
		Some(&token),
		Some(json!({
			"environment_id": env, "name": "whoami", "kind": "exec",
			"application_id": application, "command": "whoami",
			"schedule": "* * * * *", "timeout_s": 60,
		})),
	)
	.await;
	assert_eq!(s, axum::http::StatusCode::CREATED, "{whoami_task}");
	let whoami_task = whoami_task["id"].as_i64().unwrap();

	let stop = tokio_util::sync::CancellationToken::new();
	tokio::spawn(crate::jobs::worker::run_dispatcher(
		state.clone(),
		stop.clone(),
	));

	let outcome = std::panic::AssertUnwindSafe(async {
		let deadline = Instant::now() + Duration::from_secs(240);
		loop {
			let dates = runs_of(&db, date_task).await;
			let whoamis = runs_of(&db, whoami_task).await;
			let done = |runs: &[CronRun], n: usize| {
				runs.iter()
					.filter(|r| r.status == RunStatus::Succeeded)
					.count() >= n
			};
			if done(&dates, 2) && done(&whoamis, 1) {
				break;
			}
			if let Some(bad) = dates
				.iter()
				.chain(&whoamis)
				.find(|r| r.status == RunStatus::Failed)
			{
				let log = bad
					.job_id
					.and_then(|j| std::fs::read_to_string(state.logs.path(j)).ok())
					.unwrap_or_default();
				panic!("a run failed: {bad:?}\n{log}");
			}
			assert!(
				Instant::now() < deadline,
				"timed out: date {dates:?}, whoami {whoamis:?}"
			);
			tokio::time::sleep(Duration::from_secs(2)).await;
		}

		// The output is stored in the jobs' logs.
		let log_of = |run: &CronRun| {
			std::fs::read_to_string(state.logs.path(run.job_id.expect("a run has a job"))).unwrap()
		};
		let dates = runs_of(&db, date_task).await;
		let ok: Vec<&CronRun> = dates
			.iter()
			.filter(|r| r.status == RunStatus::Succeeded)
			.collect();
		assert!(ok.len() >= 2, "{dates:?}");
		for run in &ok {
			assert_eq!(run.exit_code, Some(0));
			let log = log_of(run);
			assert!(
				log.contains(" UTC "),
				"`date -u` printed its time zone: {log}"
			);
		}
		let whoamis = runs_of(&db, whoami_task).await;
		let run = whoamis
			.iter()
			.find(|r| r.status == RunStatus::Succeeded)
			.unwrap();
		assert_eq!(run.exit_code, Some(0));
		assert!(log_of(run).contains("root"), "{}", log_of(run));

		// A one-shot container is removed after its run.
		let left = podman(&[
			"ps",
			"-a",
			"--filter",
			"name=podkit-cron-",
			"--format",
			"{{.Names}}",
		]);
		let left = String::from_utf8_lossy(&left.stdout).into_owned();
		for run in &ok {
			assert!(
				!left.contains(&format!("podkit-cron-{}", run.id.0)),
				"the container of run {} is still there: {left}",
				run.id.0
			);
		}
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	// Leave nothing behind, whatever happened: only the containers of these two tasks' runs.
	stop.cancel();
	for task in [date_task, whoami_task] {
		for run in runs_of(&db, task).await {
			let _ = podman(&["rm", "-f", &format!("podkit-cron-{}", run.id.0)]);
		}
	}
	let _ = podman(&["rm", "-f", &name]);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
