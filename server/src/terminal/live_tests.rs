//! The live test of the web terminal: the real agent, in this process, and a real Podman.
//!
//! It needs a Podman API socket of its own, named by `PODKIT_LIVE_PODMAN_SOCKET` (the same
//! convention as `cron_live`), so it never touches the Podman of the person running it:
//!
//! ```text
//! podman --root R --runroot X system service --time=900 unix:///path/podman.sock &
//! PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
//!   cargo test -p server --all-features -- --ignored terminal_live
//! ```
//!
//! It starts a busybox container, opens a terminal to it through the server's WebSocket
//! (browser side), runs `echo hi` and `stty size`, and exits the shell. `docker.io/library/busybox`
//! is pulled into that Podman when it is not there (a few MB).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use proto::{TerminalControl, TerminalNotice};
use serde_json::json;
use sqlx::PgPool;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::tests::{call, state, user_with_team};

const IMAGE: &str = "docker.io/library/busybox:latest";

fn socket() -> String {
	std::env::var("PODKIT_LIVE_PODMAN_SOCKET")
		.expect("set PODKIT_LIVE_PODMAN_SOCKET to the socket of a Podman service of its own")
}

fn podman(args: &[&str]) -> std::process::Output {
	let bin = ["podman", "/run/current-system/sw/bin/podman"]
		.into_iter()
		.find(|b| {
			std::process::Command::new(b)
				.arg("--version")
				.output()
				.is_ok()
		})
		.expect("a podman binary");
	std::process::Command::new(bin)
		.arg("--url")
		.arg(format!("unix://{}", socket()))
		.args(args)
		.output()
		.unwrap()
}

/// Reads output frames until `needle` appears in what was printed, or fails after a while.
async fn read_until(
	ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
	printed: &mut String,
	needle: &str,
) {
	let found = tokio::time::timeout(Duration::from_secs(20), async {
		while !printed.contains(needle) {
			match ws.next().await {
				Some(Ok(Message::Binary(b))) => printed.push_str(&String::from_utf8_lossy(&b)),
				Some(Ok(_)) => {}
				other => {
					panic!("the terminal ended early ({other:?}); printed so far: {printed:?}")
				}
			}
		}
	})
	.await;
	assert!(found.is_ok(), "never saw {needle:?}; printed: {printed:?}");
}

#[sqlx::test]
#[ignore = "runs a real container in the Podman named by PODKIT_LIVE_PODMAN_SOCKET, pulls busybox"]
async fn terminal_live_echo_hi_comes_back_through_the_websocket(db: PgPool) {
	let state = state(db.clone());
	let socket = socket();
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
	let (_, project) = call(
		&router,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "live", "slug": "live" })),
	)
	.await;
	let project = project["id"].as_i64().unwrap();
	let server = crate::tests::server_id(&db, team).await;
	crate::tests::seed_application_on(&db, project, server).await;

	// The container the terminal opens in: a plain busybox the test starts and removes.
	let name = format!("pk-t-cd3ea927-live-{}", std::process::id());
	let started = podman(&["run", "-d", "--name", &name, IMAGE, "sleep", "600"]);
	assert!(
		started.status.success(),
		"podman run: {}",
		String::from_utf8_lossy(&started.stderr)
	);
	let outcome = std::panic::AssertUnwindSafe(async {
		crate::tests::insert_deployment(&db, 5002, 9001, "running", Some(IMAGE), Some(&name)).await;

		let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = tcp.local_addr().unwrap();
		let app = router.clone();
		tokio::spawn(async move {
			let _ = axum::serve(tcp, app).await;
		});
		let uri = format!(
			"ws://{addr}/ws/terminal/app-5002?team={team}&project={project}&cols=100&rows=30"
		);
		let mut req = uri.into_client_request().unwrap();
		req.headers_mut().insert(
			axum::http::header::AUTHORIZATION,
			format!("Bearer {token}").parse().unwrap(),
		);
		req.headers_mut().insert(
			axum::http::header::ORIGIN,
			format!("http://{addr}").parse().unwrap(),
		);
		let stream = TcpStream::connect(addr).await.unwrap();
		let (mut ws, _) = tokio_tungstenite::client_async(req, stream)
			.await
			.expect("the terminal opens");

		let mut printed = String::new();
		// A shell is there when it echoes what is typed. `echo hi` prints hi on a line of its
		// own, after the typed command line has been echoed back by the tty.
		ws.send(Message::Binary(b"echo hi\n".to_vec().into()))
			.await
			.unwrap();
		read_until(&mut ws, &mut printed, "\r\nhi\r\n").await;

		// The size announced when opening is the tty's size, and a resize changes it.
		ws.send(Message::Binary(b"stty size\n".to_vec().into()))
			.await
			.unwrap();
		read_until(&mut ws, &mut printed, "\r\n30 100\r\n").await;
		let resize = serde_json::to_string(&TerminalControl::Resize {
			cols: 120,
			rows: 40,
		})
		.unwrap();
		ws.send(Message::Text(resize.into())).await.unwrap();
		tokio::time::sleep(Duration::from_millis(300)).await;
		ws.send(Message::Binary(b"stty size\n".to_vec().into()))
			.await
			.unwrap();
		read_until(&mut ws, &mut printed, "\r\n40 120\r\n").await;

		// Ending the shell is told to the browser with its status.
		ws.send(Message::Binary(b"exit 3\n".to_vec().into()))
			.await
			.unwrap();
		let notice = tokio::time::timeout(Duration::from_secs(20), async {
			loop {
				match ws.next().await {
					Some(Ok(Message::Text(t))) => {
						break serde_json::from_str::<TerminalNotice>(t.as_str()).unwrap();
					}
					Some(Ok(_)) => {}
					other => panic!("closed without a notice: {other:?}"),
				}
			}
		})
		.await
		.expect("an exit notice");
		assert_eq!(notice, TerminalNotice::Exit { code: Some(3) });

		// Start and end are in the audit log.
		let mut actions: Vec<String> = Vec::new();
		for _ in 0..100 {
			actions = sqlx::query_scalar(
				"SELECT action FROM audit_events WHERE action LIKE 'terminal.%' ORDER BY id",
			)
			.fetch_all(&db)
			.await
			.unwrap();
			if actions.len() == 2 {
				break;
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
		assert_eq!(actions, ["terminal.start", "terminal.end"]);
		let reason: String = sqlx::query_scalar("SELECT end_reason FROM terminal_sessions")
			.fetch_one(&db)
			.await
			.unwrap();
		assert_eq!(reason, "exited");
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;
	let _ = podman(&["rm", "-f", &name]);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
