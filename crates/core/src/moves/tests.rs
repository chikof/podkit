//! Moves and drains through the API: what the plan says, what a move and a drain queue, the
//! refusals, the draining mark keeping new resources off a server, and a move cut off after
//! its switch being put back. Nothing here reaches an agent: the jobs are only queued, and
//! the parts that copy images and volumes need two live servers (see `docs/api.md`).
//!
//! `cargo test -p podkit-core -- moves`

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::ids::MoveId;
use crate::jobs::JobFailure;
use crate::state::AppState;
use crate::tests::{call, count, state, user_with_team};

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	/// The team's own server, where applications are made.
	from: i64,
	/// A second server, active.
	to: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
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
		let from = servers[0]["id"].as_i64().unwrap();
		sqlx::query("UPDATE servers SET status = 'active' WHERE id = $1")
			.bind(from)
			.execute(&db)
			.await
			.unwrap();
		let to = crypto::generate_id();
		let key = crate::tests::seed_key(&db, team, "node-02 key").await;
		sqlx::query(
			"INSERT INTO servers (id, team_id, name, hostname, ssh_user, ssh_key_id,
				podman_socket_path, is_local, status)
			VALUES ($1, $2, 'node-02', '203.0.113.2', 'root', $3, '/run/podman.sock', FALSE, 'active')",
		)
		.bind(to)
		.bind(team)
		.bind(key)
		.execute(&db)
		.await
		.unwrap();
		Self {
			app,
			state,
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
			from,
			to,
		}
	}

	fn apps(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications",
			self.team, self.project
		)
	}

	async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, path, Some(&self.token), body).await
	}

	/// An image application on the team's own server.
	async fn application(&self, slug: &str) -> i64 {
		let (s, body) = self
			.call(
				"POST",
				&self.apps(),
				Some(json!({
					"name": slug, "slug": slug, "server_id": self.from,
					"image": "traefik/whoami:v1", "container_port": 80,
				})),
			)
			.await;
		assert!(s.is_success(), "create {slug}: {s} {body}");
		body["id"].as_i64().unwrap()
	}

	/// A deployment of `app` that is live now.
	async fn running(&self, app: i64) {
		sqlx::query(
			"INSERT INTO deployments (id, application_id, status, image_tag, container_id)
			VALUES ($1, $2, 'running', 'docker.io/traefik/whoami@sha256:abc', 'c0')",
		)
		.bind(crypto::generate_id())
		.bind(app)
		.execute(&self.db)
		.await
		.unwrap();
	}

	async fn move_to(&self, app: i64, body: Value) -> (StatusCode, Value) {
		self.call("POST", &format!("{}/{app}/move", self.apps()), Some(body))
			.await
	}

	fn drain(&self, server: i64) -> String {
		format!("/api/v1/teams/{}/servers/{server}/drain", self.team)
	}
}

async fn scalar_i64(db: &PgPool, sql: &'static str, id: i64) -> i64 {
	sqlx::query_scalar(sql)
		.bind(id)
		.fetch_one(db)
		.await
		.unwrap()
}

#[sqlx::test]
async fn the_plan_judges_every_other_server(db: PgPool) {
	let s = Setup::new(db).await;
	let app = s.application("web").await;
	s.running(app).await;
	let (st, plan) = s
		.call("GET", &format!("{}/{app}/move", s.apps()), None)
		.await;
	assert_eq!(st, StatusCode::OK, "{plan}");
	assert_eq!(plan["server"]["id"], json!(s.from));
	let candidates = plan["candidates"].as_array().unwrap();
	assert_eq!(candidates.len(), 1, "{plan}");
	let c = &candidates[0];
	assert_eq!(c["server"]["id"], json!(s.to));
	assert_eq!(c["movable"], json!(true), "{c}");
	assert_eq!(c["method"], json!("redeploy"));
	assert!(c["downtime"].as_str().unwrap().starts_with("Next to none"));
	assert!(
		c["warnings"]
			.as_array()
			.unwrap()
			.iter()
			.any(|w| w.as_str().unwrap().contains("web.203-0-113-2.sslip.io")),
		"{c}"
	);
}

#[sqlx::test]
async fn a_move_is_queued_once_and_audited(db: PgPool) {
	let s = Setup::new(db).await;
	let app = s.application("web").await;
	s.running(app).await;
	let (st, mv) = s.move_to(app, json!({ "server_id": s.to })).await;
	assert_eq!(st, StatusCode::ACCEPTED, "{mv}");
	assert_eq!(mv["status"], json!("queued"));
	assert_eq!(mv["method"], json!("redeploy"));
	assert_eq!(mv["to_server"], json!("node-02"));
	let id = mv["id"].as_i64().unwrap();
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT COUNT(*) FROM jobs WHERE kind = 'application_move' AND target_id = $1",
			id
		)
		.await,
		1
	);
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT COUNT(*) FROM audit_events WHERE action = 'app.move' AND target_id = $1",
			app
		)
		.await,
		1
	);
	// The application has not moved yet: the job does that.
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT server_id FROM applications WHERE id = $1",
			app
		)
		.await,
		s.from
	);

	// A second move, and any deploy, wait for this one.
	let (st, _) = s.move_to(app, json!({ "server_id": s.to })).await;
	assert_eq!(st, StatusCode::CONFLICT);
	let (st, body) = s
		.call("POST", &format!("{}/{app}/deployments", s.apps()), None)
		.await;
	assert_eq!(st, StatusCode::CONFLICT, "{body}");

	let (st, history) = s
		.call("GET", &format!("{}/{app}/moves", s.apps()), None)
		.await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(history[0]["id"], json!(id));
}

#[sqlx::test]
async fn refusals_say_why(db: PgPool) {
	let s = Setup::new(db).await;
	let app = s.application("web").await;

	// Its own server.
	let (st, body) = s.move_to(app, json!({ "server_id": s.from })).await;
	assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");

	// A target that is not ready.
	sqlx::query("UPDATE servers SET status = 'unreachable' WHERE id = $1")
		.bind(s.to)
		.execute(&s.db)
		.await
		.unwrap();
	let (st, body) = s.move_to(app, json!({ "server_id": s.to })).await;
	assert_eq!(st, StatusCode::BAD_REQUEST);
	assert!(body.to_string().contains("not ready"), "{body}");
	sqlx::query("UPDATE servers SET status = 'active' WHERE id = $1")
		.bind(s.to)
		.execute(&s.db)
		.await
		.unwrap();

	// Volumes without a destination.
	sqlx::query(
		"INSERT INTO volumes (id, application_id, name, mount_path) VALUES ($1, $2, 'podkit-vol-1', '/data')",
	)
	.bind(crypto::generate_id())
	.bind(app)
	.execute(&s.db)
	.await
	.unwrap();
	let (st, body) = s.move_to(app, json!({ "server_id": s.to })).await;
	assert_eq!(st, StatusCode::BAD_REQUEST);
	assert!(body.to_string().contains("backup destination"), "{body}");

	// A server that is not the team's is not found.
	let (st, _) = s.move_to(app, json!({ "server_id": 424_242 })).await;
	assert_eq!(st, StatusCode::NOT_FOUND);
	assert_eq!(count(&s.db, "application_moves").await, 0);
}

#[sqlx::test]
async fn a_read_token_cannot_move(db: PgPool) {
	let s = Setup::new(db).await;
	let app = s.application("web").await;
	let (_, made) = s
		.call(
			"POST",
			&format!("/api/v1/teams/{}/api-tokens", s.team),
			Some(json!({ "name": "ro", "scopes": ["read"] })),
		)
		.await;
	let token = made["secret"].as_str().unwrap();
	let path = format!("{}/{app}/move", s.apps());
	let (st, _) = call(&s.app, "GET", &path, Some(token), None).await;
	assert_eq!(st, StatusCode::OK);
	let (st, _) = call(
		&s.app,
		"POST",
		&path,
		Some(token),
		Some(json!({ "server_id": s.to })),
	)
	.await;
	assert_eq!(st, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn draining_keeps_new_resources_off_and_queues_one_drain(db: PgPool) {
	let s = Setup::new(db).await;
	let app = s.application("web").await;
	s.running(app).await;

	let (st, plan) = s
		.call(
			"GET",
			&format!("{}?target_server_id={}", s.drain(s.from), s.to),
			None,
		)
		.await;
	assert_eq!(st, StatusCode::OK, "{plan}");
	assert_eq!(plan["items"][0]["kind"], json!("application"));
	assert_eq!(plan["items"][0]["outcome"], json!("pending"));
	assert_eq!(plan["items"][0]["verdict"]["method"], json!("redeploy"));

	let (st, drain) = s
		.call(
			"POST",
			&s.drain(s.from),
			Some(json!({ "target_server_id": s.to })),
		)
		.await;
	assert_eq!(st, StatusCode::ACCEPTED, "{drain}");
	assert_eq!(drain["status"], json!("queued"));
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT COUNT(*) FROM jobs WHERE kind = 'server_drain' AND target_id = $1",
			drain["id"].as_i64().unwrap()
		)
		.await,
		1
	);
	let (st, _) = s
		.call(
			"POST",
			&s.drain(s.from),
			Some(json!({ "target_server_id": s.to })),
		)
		.await;
	assert_eq!(st, StatusCode::CONFLICT);

	// Nothing new goes to a draining server, and nothing moves to one either.
	let (st, body) = s
		.call(
			"POST",
			&s.apps(),
			Some(json!({
				"name": "api", "slug": "api", "server_id": s.from,
				"image": "traefik/whoami:v1", "container_port": 80,
			})),
		)
		.await;
	assert_eq!(st, StatusCode::CONFLICT, "{body}");
	let (st, _) = s
		.call(
			"POST",
			&s.drain(s.to),
			Some(json!({ "target_server_id": s.from })),
		)
		.await;
	assert_eq!(st, StatusCode::BAD_REQUEST);

	let (st, _) = s
		.call(
			"POST",
			&format!("/api/v1/teams/{}/servers/{}/undrain", s.team, s.from),
			None,
		)
		.await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT COUNT(*) FROM servers WHERE id = $1 AND draining_since IS NOT NULL",
			s.from
		)
		.await,
		0
	);
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT COUNT(*) FROM audit_events
			WHERE action IN ('server.drain', 'server.undrain') AND target_id = $1",
			s.from
		)
		.await,
		2
	);
}

#[sqlx::test]
async fn a_move_cut_off_after_its_switch_is_put_back(db: PgPool) {
	let s = Setup::new(db).await;
	let app = s.application("web").await;
	let (_, mv) = s.move_to(app, json!({ "server_id": s.to })).await;
	let id = mv["id"].as_i64().unwrap();
	let switched = crate::moves::switch(
		&s.db,
		MoveId(id),
		crate::ids::ApplicationId(app),
		crate::ids::ServerId(s.from),
		crate::ids::ServerId(s.to),
	)
	.await
	.unwrap();
	assert!(switched);
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT server_id FROM applications WHERE id = $1",
			app
		)
		.await,
		s.to
	);

	crate::moves::fail_move(
		&s.state,
		id,
		&JobFailure::Failed("interrupted: podkit restarted".to_owned()),
	)
	.await
	.unwrap();
	assert_eq!(
		scalar_i64(
			&s.db,
			"SELECT server_id FROM applications WHERE id = $1",
			app
		)
		.await,
		s.from
	);
	let (status, step): (String, String) =
		sqlx::query_as("SELECT status, step FROM application_moves WHERE id = $1")
			.bind(id)
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert_eq!(status, "failed");
	assert!(step.contains("put back"), "{step}");
}
