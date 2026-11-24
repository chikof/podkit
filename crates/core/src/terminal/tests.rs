//! The web terminal end to end, against a fake agent: who may open it (session, permission,
//! tenancy, origin, token scope), what crosses the socket, and what is written down.
//!
//! The pump's own cases (resize clamping, idle timeout, transcript cap) are in `pump.rs`;
//! here the pieces are wired through the real routes and a real WebSocket.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures_util::{SinkExt, StreamExt};
use proto::TerminalControl;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

use crate::tests::{self, call, db_test, notifications_ui::add_member, user_with_team};

/// What the fake agent saw: the request line of each connection, and each text frame.
#[derive(Default)]
struct AgentLog {
	uris: Vec<String>,
	texts: Vec<String>,
}

struct World {
	app: Router,
	db: PgPool,
	session: String,
	team: i64,
	project: i64,
	log: Arc<Mutex<AgentLog>>,
}

const CONTAINER: &str = "ctr-terminal-1";

impl World {
	/// Alice owns a team with one project and an application (5002) whose newest deployment
	/// is running in `CONTAINER`; a fake agent answers on the local server's socket.
	async fn new(db: PgPool) -> Self {
		let state = tests::state(db.clone());
		let socket = state.config.local_agent_socket.clone();
		let app = crate::routes::routes(state).await.unwrap();
		let (session, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&session),
			Some(json!({ "name": "Web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "project: {s}");
		let project = project["id"].as_i64().unwrap();
		let server = tests::server_id(&db, team).await;
		tests::seed_application_on(&db, project, server).await;
		tests::insert_deployment(&db, 5002, 9001, "running", Some("img"), Some(CONTAINER)).await;

		let log = Arc::new(Mutex::new(AgentLog::default()));
		fake_agent(&socket, Arc::clone(&log));
		Self {
			app,
			db,
			session,
			team,
			project,
			log,
		}
	}

	fn path(&self) -> String {
		format!(
			"/ws/terminal/app-5002?team={}&project={}&cols=100&rows=30",
			self.team, self.project
		)
	}

	/// A plain `GET` of a terminal path (no upgrade): what the guards answer.
	async fn get(&self, uri: &str, bearer: Option<&str>, headers: &[(&str, &str)]) -> StatusCode {
		let mut req = Request::builder().method("GET").uri(uri);
		if let Some(t) = bearer {
			req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
		}
		for (k, v) in headers {
			req = req.header(*k, *v);
		}
		self.app
			.clone()
			.oneshot(req.body(Body::empty()).unwrap())
			.await
			.unwrap()
			.status()
	}

	/// Serves the routes on a real port and opens the terminal as `bearer`.
	async fn open(
		&self,
		uri: &str,
		bearer: &str,
	) -> Result<tokio_tungstenite::WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error>
	{
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let app = self.app.clone();
		tokio::spawn(async move {
			let _ = axum::serve(listener, app).await;
		});
		let mut req = format!("ws://{addr}{uri}").into_client_request().unwrap();
		req.headers_mut().insert(
			header::AUTHORIZATION,
			format!("Bearer {bearer}").parse().unwrap(),
		);
		req.headers_mut()
			.insert(header::ORIGIN, format!("http://{addr}").parse().unwrap());
		let stream = TcpStream::connect(addr).await?;
		tokio_tungstenite::client_async(req, stream)
			.await
			.map(|(ws, _)| ws)
	}

	async fn actions(&self) -> Vec<String> {
		sqlx::query_scalar(
			"SELECT action FROM audit_events WHERE action LIKE 'terminal.%' ORDER BY id",
		)
		.fetch_all(&self.db)
		.await
		.unwrap()
	}

	/// Waits for the session's end to be written (it happens after the socket closes).
	async fn wait_for_end(&self) {
		for _ in 0..100 {
			if self.actions().await.iter().any(|a| a == "terminal.end") {
				return;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
		panic!("the session never ended");
	}
}

/// Listens on `socket` and plays the agent: every binary frame comes back as `out:<bytes>`,
/// text frames are recorded.
/// The handshake callback's `Err` type is fixed by tungstenite's `Callback` trait.
#[allow(clippy::result_large_err)]
fn fake_agent(socket: &std::path::Path, log: Arc<Mutex<AgentLog>>) {
	let listener = UnixListener::bind(socket).unwrap();
	tokio::spawn(async move {
		loop {
			let Ok((stream, _)) = listener.accept().await else {
				return;
			};
			let log = Arc::clone(&log);
			tokio::spawn(async move {
				let uri = Arc::new(Mutex::new(String::new()));
				let seen = Arc::clone(&uri);
				let ws = tokio_tungstenite::accept_hdr_async(
					stream,
					move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
					      resp| {
						*seen.lock().unwrap() = req.uri().to_string();
						Ok(resp)
					},
				)
				.await;
				let Ok(mut ws) = ws else { return };
				let line = uri.lock().unwrap().clone();
				log.lock().unwrap().uris.push(line);
				while let Some(Ok(frame)) = ws.next().await {
					match frame {
						Message::Binary(b) => {
							let mut out = b"out:".to_vec();
							out.extend_from_slice(&b);
							if ws.send(Message::Binary(out.into())).await.is_err() {
								return;
							}
						}
						Message::Text(t) => log.lock().unwrap().texts.push(t.as_str().to_owned()),
						Message::Close(_) => return,
						_ => {}
					}
				}
			});
		}
	});
}

// --- who may open it ---------------------------------------------------------------------

#[db_test]
async fn no_session_is_401(db: PgPool) {
	let w = World::new(db).await;
	assert_eq!(w.get(&w.path(), None, &[]).await, StatusCode::UNAUTHORIZED);
	assert_eq!(
		w.get(&w.path(), Some("not-a-session"), &[]).await,
		StatusCode::UNAUTHORIZED
	);
	assert!(w.actions().await.is_empty());
}

#[db_test]
async fn a_member_without_the_permission_is_403(db: PgPool) {
	let w = World::new(db).await;
	let (bob, _) = user_with_team(&w.app, "bob").await;
	add_member(&w.db, w.team, "bob").await;
	assert_eq!(
		w.get(&w.path(), Some(&bob), &[]).await,
		StatusCode::FORBIDDEN
	);
	assert!(w.actions().await.is_empty());
}

#[db_test]
async fn another_teams_member_gets_404_for_every_way_in(db: PgPool) {
	let w = World::new(db).await;
	let (bob, bob_team) = user_with_team(&w.app, "bob").await;
	// Alice's team, which bob is not in.
	assert_eq!(
		w.get(&w.path(), Some(&bob), &[]).await,
		StatusCode::NOT_FOUND
	);
	// His own team, but alice's project and application.
	let own = format!(
		"/ws/terminal/app-5002?team={bob_team}&project={}",
		w.project
	);
	assert_eq!(w.get(&own, Some(&bob), &[]).await, StatusCode::NOT_FOUND);
	// A path that is no resource at all is the same answer.
	for bad in ["app-x", "host-1", "stack-1", "app-0"] {
		let uri = format!("/ws/terminal/{bad}?team={}&project={}", w.team, w.project);
		assert_eq!(
			w.get(&uri, Some(&w.session), &[]).await,
			StatusCode::NOT_FOUND,
			"{bad}"
		);
	}
	assert!(w.actions().await.is_empty());
}

#[db_test]
async fn a_wrong_origin_is_403_before_anything_else(db: PgPool) {
	let w = World::new(db).await;
	// A good session, from another site.
	let cross = [("host", "podkit.test"), ("origin", "https://evil.example")];
	assert_eq!(
		w.get(&w.path(), Some(&w.session), &cross).await,
		StatusCode::FORBIDDEN
	);
	// Even with no session at all, the origin is what is refused first.
	assert_eq!(w.get(&w.path(), None, &cross).await, StatusCode::FORBIDDEN);
	// The browser says it is another site and sends no origin.
	let site = [("host", "podkit.test"), ("sec-fetch-site", "cross-site")];
	assert_eq!(
		w.get(&w.path(), Some(&w.session), &site).await,
		StatusCode::FORBIDDEN
	);
	// The same site passes the guard (and is then only refused for not being an upgrade).
	let same = [("host", "podkit.test"), ("origin", "http://podkit.test")];
	assert_ne!(
		w.get(&w.path(), Some(&w.session), &same).await,
		StatusCode::FORBIDDEN
	);
	assert!(w.actions().await.is_empty());
}

#[db_test]
async fn an_api_token_needs_root(db: PgPool) {
	let w = World::new(db).await;
	let make = |name: &'static str, scopes: Value| {
		let app = w.app.clone();
		let session = w.session.clone();
		let team = w.team;
		async move {
			let (s, made) = call(
				&app,
				"POST",
				&format!("/api/v1/teams/{team}/api-tokens"),
				Some(&session),
				Some(json!({ "name": name, "scopes": scopes })),
			)
			.await;
			assert!(s.is_success(), "{s} {made}");
			made["secret"].as_str().unwrap().to_owned()
		}
	};
	let write = make("narrow", json!(["read", "write", "deploy", "delete"])).await;
	assert_eq!(
		w.get(&w.path(), Some(&write), &[]).await,
		StatusCode::FORBIDDEN
	);
	assert!(w.actions().await.is_empty());

	// A root token gets in, and the audit log says a token did it.
	let root = make("full", json!(["root"])).await;
	let mut ws = w.open(&w.path(), &root).await.expect("root token opens");
	ws.close(None).await.unwrap();
	w.wait_for_end().await;
	let token: Option<i64> = sqlx::query_scalar(
		"SELECT actor_token_id FROM audit_events WHERE action = 'terminal.start'",
	)
	.fetch_one(&w.db)
	.await
	.unwrap();
	assert!(token.is_some(), "a token session is logged with its token");
}

#[db_test]
async fn a_stopped_application_is_a_409(db: PgPool) {
	let w = World::new(db).await;
	tests::set_deployment_status(&w.db, 9001, "stopped").await;
	assert_eq!(
		w.get(&w.path(), Some(&w.session), &[]).await,
		StatusCode::CONFLICT
	);
	assert!(w.actions().await.is_empty());
}

// --- a session ---------------------------------------------------------------------------

#[db_test]
async fn bytes_and_resizes_cross_and_start_and_end_are_audited(db: PgPool) {
	let w = World::new(db).await;
	let mut ws = w.open(&w.path(), &w.session).await.expect("opens");

	ws.send(Message::Binary(b"echo hi\n".to_vec().into()))
		.await
		.unwrap();
	let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(reply, Message::Binary(b"out:echo hi\n".to_vec().into()));

	// A resize reaches the agent, clamped; other text does not.
	let resize = serde_json::to_string(&TerminalControl::Resize {
		cols: 9999,
		rows: 40,
	})
	.unwrap();
	ws.send(Message::Text(resize.into())).await.unwrap();
	ws.send(Message::Text("{\"type\":\"rm -rf\"}".into()))
		.await
		.unwrap();
	ws.send(Message::Binary(b"x".to_vec().into()))
		.await
		.unwrap();
	let _ = ws.next().await; // the echo of "x": the text frames before it were handled
	ws.close(None).await.unwrap();
	w.wait_for_end().await;

	{
		let seen = w.log.lock().unwrap();
		assert_eq!(seen.uris.len(), 1);
		assert!(
			seen.uris[0].contains(&format!("/v1/containers/{CONTAINER}/terminal")),
			"{:?}",
			seen.uris
		);
		assert!(seen.uris[0].contains("cols=100") && seen.uris[0].contains("rows=30"));
		assert_eq!(seen.texts.len(), 1, "{:?}", seen.texts);
		let sent: TerminalControl = serde_json::from_str(&seen.texts[0]).unwrap();
		let TerminalControl::Resize { cols, rows } = sent;
		assert_eq!(rows, 40);
		assert!((1..=proto::TERMINAL_MAX_COLS).contains(&cols), "{cols}");
	}

	// Start, then end; both name the person and the resource; no keystrokes anywhere.
	assert_eq!(w.actions().await, ["terminal.start", "terminal.end"]);
	let rows: Vec<(String, Value, Option<i64>, String)> = sqlx::query_as(
		"SELECT action, diff, actor_user_id, summary FROM audit_events
		 WHERE action LIKE 'terminal.%' ORDER BY id",
	)
	.fetch_all(&w.db)
	.await
	.unwrap();
	let alice = tests::user_id(&w.db, "alice").await;
	for (_, _, actor, _) in &rows {
		assert_eq!(*actor, Some(alice));
	}
	assert_eq!(rows[0].3, "Opened terminal in application Pinned");
	assert_eq!(rows[1].3, "Closed terminal in application Pinned");
	assert_eq!(rows[1].1["reason"], "closed");
	assert!(rows[1].1["bytes_typed"].as_i64().unwrap() >= 9);
	assert!(
		!format!("{rows:?}").contains("echo hi"),
		"keystrokes reached the log"
	);

	// The session row is closed and, with recording off, keeps no transcript.
	let (reason, transcript): (String, Option<Vec<u8>>) =
		sqlx::query_as("SELECT end_reason, transcript FROM terminal_sessions WHERE team_id = $1")
			.bind(w.team)
			.fetch_one(&w.db)
			.await
			.unwrap();
	assert_eq!(reason, "closed");
	assert!(transcript.is_none());
}

#[db_test]
async fn the_team_option_records_the_output(db: PgPool) {
	let w = World::new(db).await;
	let (s, _) = call(
		&w.app,
		"PUT",
		&format!("/api/v1/teams/{}/terminal/recording", w.team),
		Some(&w.session),
		Some(json!({ "enabled": true })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);

	let mut ws = w.open(&w.path(), &w.session).await.unwrap();
	ws.send(Message::Binary(b"typed\n".to_vec().into()))
		.await
		.unwrap();
	let _ = ws.next().await;
	ws.close(None).await.unwrap();
	w.wait_for_end().await;

	let (_, sessions) = call(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{}/terminal/sessions", w.team),
		Some(&w.session),
		None,
	)
	.await;
	assert_eq!(sessions[0]["has_transcript"], true, "{sessions}");
	let id = sessions[0]["id"].as_i64().unwrap();
	let resp = w
		.app
		.clone()
		.oneshot(
			Request::builder()
				.uri(format!(
					"/api/v1/teams/{}/terminal/sessions/{id}/transcript",
					w.team
				))
				.header(header::AUTHORIZATION, format!("Bearer {}", w.session))
				.body(Body::empty())
				.unwrap(),
		)
		.await
		.unwrap();
	assert_eq!(resp.status(), StatusCode::OK);
	let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	// The recorder keeps what came from the container (the fake echoes with a prefix).
	assert_eq!(String::from_utf8_lossy(&body), "out:typed\n");

	// Turning it on and reading it back were both audited.
	let actions = w.actions().await;
	assert!(
		actions.contains(&"terminal.transcript".to_owned()),
		"{actions:?}"
	);
	let on: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = 'terminal.record'")
			.fetch_one(&w.db)
			.await
			.unwrap();
	assert_eq!(on, 1);
}

#[db_test]
async fn only_someone_who_may_update_the_team_changes_the_option(db: PgPool) {
	let w = World::new(db).await;
	let (bob, _) = user_with_team(&w.app, "bob").await;
	add_member(&w.db, w.team, "bob").await;
	let uri = format!("/api/v1/teams/{}/terminal/recording", w.team);
	let (s, _) = call(
		&w.app,
		"PUT",
		&uri,
		Some(&bob),
		Some(json!({ "enabled": true })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, on) = call(&w.app, "GET", &uri, Some(&bob), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(on["enabled"], false);
	// A stranger cannot even see it.
	let (carol, _) = user_with_team(&w.app, "carol").await;
	let (s, _) = call(&w.app, "GET", &uri, Some(&carol), None).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn a_database_terminal_opens_in_its_container(db: PgPool) {
	let w = World::new(db).await;
	tests::seed_app_and_database(&w.db, w.team, w.project).await;
	tests::dbs::set_state(&w.db, 6001, "running", Some("dbctr")).await;
	let uri = format!(
		"/ws/terminal/database-6001?team={}&project={}",
		w.team, w.project
	);
	let mut ws = w.open(&uri, &w.session).await.unwrap();
	ws.send(Message::Binary(b"\n".to_vec().into()))
		.await
		.unwrap();
	let _ = ws.next().await;
	ws.close(None).await.unwrap();
	w.wait_for_end().await;
	let uris = w.log.lock().unwrap().uris.clone();
	assert!(
		uris[0].contains("/v1/containers/dbctr/terminal"),
		"{uris:?}"
	);
	let summary: String =
		sqlx::query_scalar("SELECT summary FROM audit_events WHERE action = 'terminal.start'")
			.fetch_one(&w.db)
			.await
			.unwrap();
	assert_eq!(summary, "Opened terminal in database orders");
}
