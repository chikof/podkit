//! Image auto-update against a fake agent on the local server's socket: the settings API and
//! its checks, the scheduled check that deploys a moved tag through the normal deploy, the
//! digest held back after a failed or rolled-back auto-update, freezes, and the label on the
//! deploy spec. The pure decisions are tested in [`super::rules`].

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;

use super::engine::{self, Trigger};
use crate::applications;
use crate::deployments;
use crate::ids::{ApplicationId, UserId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::tests::{call, count, join_as_project_reader, state, user_id, user_with_team};

fn digest(c: char) -> String {
	format!("sha256:{}", c.to_string().repeat(64))
}

/// What the fake agent answers: the digest the tag names, and every deploy spec it got.
#[derive(Clone, Default)]
struct Fake {
	remote: Arc<Mutex<String>>,
	specs: Arc<Mutex<Vec<Value>>>,
}

fn fake_agent(state: &AppState) -> Fake {
	let fake = Fake::default();
	*fake.remote.lock().unwrap() = digest('a');
	let (remote, specs) = (fake.remote.clone(), fake.specs.clone());
	let app = Router::new()
		.route(
			"/v1/deploy",
			post(move |body: Bytes| async move {
				let spec: Value = serde_json::from_slice(&body).unwrap();
				specs.lock().unwrap().push(spec);
				// After a pull the agent reports the digest reference that runs.
				let image = format!("ghcr.io/me/app@{}", digest('a'));
				let events = [
					json!({ "type": "deployed", "container_id": "c1", "image": image,
					"commit_sha": null }),
				];
				let body = events.iter().fold(String::new(), |mut out, e| {
					out.push_str(&e.to_string());
					out.push('\n');
					out
				});
				([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
			}),
		)
		.route(
			"/v1/image-digest",
			post(move |body: Bytes| async move {
				let req: Value = serde_json::from_slice(&body).unwrap();
				assert_eq!(req["reference"], "ghcr.io/me/app:latest");
				let d = remote.lock().unwrap().clone();
				axum::Json(json!({ "digest": d, "local_digests": [] })).into_response()
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	fake
}

struct Rig {
	st: AppState,
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	server: i64,
}

impl Rig {
	async fn new(db: PgPool) -> Self {
		let st = state(db.clone());
		let app = crate::routes::routes(st.clone()).await.unwrap();
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
			st,
			app,
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

	async fn create(&self, slug: &str, source: Value) -> i64 {
		let mut body = json!({
			"name": slug, "slug": slug, "server_id": self.server, "container_port": 80,
		});
		for (k, v) in source.as_object().unwrap() {
			body[k] = v.clone();
		}
		let (s, a) = call(
			&self.app,
			"POST",
			&self.apps(),
			Some(&self.token),
			Some(body),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{a}");
		a["id"].as_i64().unwrap()
	}

	fn url(&self, app: i64) -> String {
		format!("{}/{app}/auto-update", self.apps())
	}

	async fn put(&self, app: i64, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"PUT",
			&self.url(app),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	/// Deploys the application the way the API does and runs its job the way the worker does.
	async fn deploy(&self, app: i64) {
		let stored = applications::find_by_id(&self.db, ApplicationId(app))
			.await
			.unwrap()
			.unwrap();
		let queued = deployments::queue(&self.st, &stored, None, None)
			.await
			.unwrap();
		self.run(queued.id.0).await;
	}

	async fn run(&self, deployment: i64) {
		let job = jobs::latest_for(&self.db, JobKind::Deploy, deployment)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.st.logs.open(job.id).await.unwrap();
		deployments::run::run(&self.st, &job, &CancellationToken::new(), &mut log)
			.await
			.unwrap_or_else(|e| panic!("deploy failed: {e:?}"));
		log.finish().await;
	}

	async fn check(&self, app: i64) -> super::Row {
		engine::check(&self.st, ApplicationId(app), Trigger::Schedule)
			.await
			.unwrap()
			.expect("a row")
	}
}

#[sqlx::test]
async fn auto_update_is_off_until_turned_on_and_settings_round_trip(db: PgPool) {
	let r = Rig::new(db).await;
	let app = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;

	let (s, body) = call(&r.app, "GET", &r.url(app), Some(&r.token), None).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["enabled"], false);
	assert_eq!(body["interval"], "daily");
	assert_eq!(body["window"], Value::Null);
	assert_eq!(body["last_result"], Value::Null);

	let (s, body) = r
		.put(
			app,
			json!({ "enabled": true, "interval": "6h",
				"window": { "start": "02:00", "end": "04:30", "utc_offset_minutes": 60 } }),
		)
		.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["enabled"], true);
	assert_eq!(body["interval"], "6h");
	assert_eq!(body["window"]["start"], "02:00");
	assert_eq!(body["window"]["end"], "04:30");
	assert_eq!(body["window"]["utc_offset_minutes"], 60);
	assert!(body["next_check_at"].is_string(), "{body}");

	let (_, audit) = call(
		&r.app,
		"GET",
		&format!("/api/v1/teams/{}/audit", r.team),
		Some(&r.token),
		None,
	)
	.await;
	assert!(audit.to_string().contains("app.auto_update"), "{audit}");

	let (s, body) = r.put(app, json!({ "enabled": false })).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["enabled"], false);
	assert_eq!(body["next_check_at"], Value::Null);
}

#[sqlx::test]
async fn auto_update_refuses_what_it_cannot_follow(db: PgPool) {
	let r = Rig::new(db).await;
	let git = r
		.create(
			"site",
			json!({ "repo_url": "https://github.com/traefik/whoami", "git_ref": "master" }),
		)
		.await;
	let (s, body) = r.put(git, json!({ "enabled": true })).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");

	let pinned = r
		.create(
			"pinned",
			json!({ "image": format!("ghcr.io/me/app@{}", digest('a')) }),
		)
		.await;
	let (s, body) = r.put(pinned, json!({ "enabled": true })).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert!(body.to_string().contains("pinned"), "{body}");

	let img = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;
	for bad in [
		json!({ "enabled": true, "interval": "weekly" }),
		json!({ "enabled": true, "window": { "start": "25:00", "end": "01:00" } }),
		json!({ "enabled": true, "window": { "start": "02:00", "end": "02:00" } }),
		json!({ "enabled": true, "window": { "start": "02:00", "end": "03:00", "utc_offset_minutes": 9999 } }),
	] {
		let (s, body) = r.put(img, bad.clone()).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{bad} {body}");
	}

	// Check now needs auto-update on.
	let (s, body) = call(
		&r.app,
		"POST",
		&format!("{}/check", r.url(img)),
		Some(&r.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
}

#[sqlx::test]
async fn auto_update_follows_the_team_and_permission_rules(db: PgPool) {
	let r = Rig::new(db).await;
	let img = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;
	let (bob, _) = user_with_team(&r.app, "bob").await;
	let (s, _) = call(&r.app, "GET", &r.url(img), Some(&bob), None).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, _) = call(
		&r.app,
		"PUT",
		&r.url(img),
		Some(&bob),
		Some(json!({ "enabled": true })),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);

	// Carol may read applications in the team and nothing more.
	let (reader, _) = user_with_team(&r.app, "carol").await;
	let carol = user_id(&r.db, "carol").await;
	sqlx::query("DELETE FROM team_members WHERE user_id = $1")
		.bind(carol)
		.execute(&r.db)
		.await
		.unwrap();
	join_as_project_reader(&r.db, r.team, carol, 9_100).await;
	sqlx::query(
		"INSERT INTO role_permissions (role_id, permission_id)
		 SELECT $1, id FROM permissions WHERE action = 'read' AND resource = 'application'",
	)
	.bind(9_100_i64)
	.execute(&r.db)
	.await
	.unwrap();
	let (s, _) = call(&r.app, "GET", &r.url(img), Some(&reader), None).await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&r.app,
		"PUT",
		&r.url(img),
		Some(&reader),
		Some(json!({ "enabled": true })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = call(
		&r.app,
		"POST",
		&format!("{}/check", r.url(img)),
		Some(&reader),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn auto_update_deploys_a_moved_tag_and_holds_back_a_bad_digest(db: PgPool) {
	let r = Rig::new(db).await;
	let fake = fake_agent(&r.st);
	let app = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;
	r.deploy(app).await;
	let (s, body) = r
		.put(app, json!({ "enabled": true, "interval": "hourly" }))
		.await;
	assert_eq!(s, StatusCode::OK, "{body}");

	// The deploy spec carries the label request once auto-update is on.
	r.deploy(app).await;
	let specs = fake.specs.lock().unwrap().clone();
	assert_eq!(specs[0].get("auto_update"), None);
	assert_eq!(specs[1]["auto_update"], true);

	// The tag names what runs: nothing to do.
	let before = count(&r.db, "deployments").await;
	let row = r.check(app).await;
	assert_eq!(row.last_result.as_deref(), Some("up_to_date"));
	assert_eq!(count(&r.db, "deployments").await, before);
	let next = row.next_check_at.unwrap();
	assert!(
		next > OffsetDateTime::now_utc() + Duration::minutes(50),
		"{next}"
	);

	// The tag moves: a deployment is queued through the normal path, and the audit log names
	// the trigger.
	*fake.remote.lock().unwrap() = digest('b');
	let row = r.check(app).await;
	assert_eq!(
		row.last_result.as_deref(),
		Some("deployed"),
		"{}",
		row.last_detail
	);
	assert_eq!(count(&r.db, "deployments").await, before + 1);
	assert_eq!(row.deployed_digest, Some(digest('b')));
	let deployment = row.deployment_id.unwrap();
	let audit: Vec<String> =
		sqlx::query_scalar("SELECT diff::text FROM audit_events WHERE action = 'app.deploy'")
			.fetch_all(&r.db)
			.await
			.unwrap();
	assert!(audit.iter().any(|d| d.contains("auto-update")), "{audit:?}");

	// While it is queued, another check waits instead of queueing a second one.
	let row = r.check(app).await;
	assert_eq!(row.last_result.as_deref(), Some("busy"));
	assert_eq!(count(&r.db, "deployments").await, before + 1);

	// It fails (the health gate, a crash, a rollback all count the same).
	sqlx::query("UPDATE deployments SET status = 'failed', finished_at = NOW() WHERE id = $1")
		.bind(deployment)
		.execute(&r.db)
		.await
		.unwrap();
	let events = count(&r.db, "notification_events").await;
	let row = r.check(app).await;
	assert_eq!(
		row.last_result.as_deref(),
		Some("skipped_bad"),
		"{}",
		row.last_detail
	);
	assert_eq!(row.bad_digest, Some(digest('b')));
	assert_eq!(count(&r.db, "deployments").await, before + 1, "no loop");
	assert!(count(&r.db, "notification_events").await > events);
	// And again: still held back, told once.
	let events = count(&r.db, "notification_events").await;
	let row = r.check(app).await;
	assert_eq!(row.last_result.as_deref(), Some("skipped_bad"));
	assert_eq!(count(&r.db, "notification_events").await, events);

	// The tag moves on: the hold is forgotten and the new digest deploys.
	*fake.remote.lock().unwrap() = digest('c');
	let row = r.check(app).await;
	assert_eq!(row.last_result.as_deref(), Some("deployed"));
	assert_eq!(row.bad_digest, None);
	assert_eq!(row.deployed_digest, Some(digest('c')));
}

#[sqlx::test]
async fn auto_update_waits_for_a_freeze_and_the_scheduler_claims_due_checks(db: PgPool) {
	let r = Rig::new(db).await;
	let fake = fake_agent(&r.st);
	let app = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;
	r.deploy(app).await;
	r.put(app, json!({ "enabled": true })).await;
	*fake.remote.lock().unwrap() = digest('b');

	let now = OffsetDateTime::now_utc();
	sqlx::query(
		"INSERT INTO autodeploy_freezes (application_id, kind, starts_at, ends_at) \
		 VALUES ($1, 'once', $2, $3)",
	)
	.bind(app)
	.bind(now - Duration::hours(1))
	.bind(now + Duration::hours(1))
	.execute(&r.db)
	.await
	.unwrap();
	let before = count(&r.db, "deployments").await;
	let row = r.check(app).await;
	assert_eq!(row.last_result.as_deref(), Some("frozen"));
	assert_eq!(count(&r.db, "deployments").await, before);
	// A freeze retries soon, not a day later.
	assert!(row.next_check_at.unwrap() < now + Duration::minutes(30));

	sqlx::query("DELETE FROM autodeploy_freezes")
		.execute(&r.db)
		.await
		.unwrap();
	// Not due yet: the pass leaves it alone.
	engine::sweep(&r.st, now).await;
	assert_eq!(count(&r.db, "deployments").await, before);
	// Due: one pass checks it and deploys.
	engine::sweep(&r.st, now + Duration::hours(1)).await;
	assert_eq!(count(&r.db, "deployments").await, before + 1);
	let row = super::get(&r.db, ApplicationId(app))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(row.last_result.as_deref(), Some("deployed"));
}

#[sqlx::test]
async fn auto_update_check_now_deploys_as_the_user_and_is_audited(db: PgPool) {
	let r = Rig::new(db).await;
	let fake = fake_agent(&r.st);
	let app = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;
	r.deploy(app).await;
	// A window that is never open now: Check now does not wait for it.
	let closed = (OffsetDateTime::now_utc() + Duration::hours(6)).hour();
	let start = format!("{closed:02}:00");
	let end = format!("{:02}:00", (closed + 1) % 24);
	let (s, body) = r
		.put(
			app,
			json!({ "enabled": true, "window": { "start": start, "end": end } }),
		)
		.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	*fake.remote.lock().unwrap() = digest('b');

	let (s, body) = call(
		&r.app,
		"POST",
		&format!("{}/check", r.url(app)),
		Some(&r.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["last_result"], "deployed", "{body}");
	assert_eq!(body["remote_digest"], digest('b'));
	let alice = user_id(&r.db, "alice").await;
	let by: Option<i64> = sqlx::query_scalar("SELECT triggered_by FROM deployments WHERE id = $1")
		.bind(body["deployment_id"].as_i64().unwrap())
		.fetch_one(&r.db)
		.await
		.unwrap();
	assert_eq!(by.map(UserId), Some(UserId(alice)));
	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events")
		.fetch_all(&r.db)
		.await
		.unwrap();
	assert!(
		actions.iter().any(|a| a == "app.auto_update.check"),
		"{actions:?}"
	);
}

#[sqlx::test]
async fn auto_update_records_an_unreachable_agent_as_a_result(db: PgPool) {
	let r = Rig::new(db).await;
	let app = r
		.create("img", json!({ "image": "ghcr.io/me/app:latest" }))
		.await;
	r.put(app, json!({ "enabled": true })).await;
	let row = r.check(app).await;
	assert_eq!(row.last_result.as_deref(), Some("error"));
	assert!(
		row.last_detail.starts_with("Could not check"),
		"{}",
		row.last_detail
	);
	assert!(row.next_check_at.is_some());
}
