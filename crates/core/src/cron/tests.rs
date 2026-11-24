//! Scheduled tasks: runs, concurrency, timeouts, retries, catch-up, the leader lock,
//! notifications and tenancy.
//! `cargo test -p podkit-core --all-features -- cron`

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use proto::{Event, Stream as LogStream};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::run::{self, Outcome};
use super::{RunStatus, Trigger, scheduler, store};
use crate::error::ServerError;
use crate::ids::{CronRunId, CronTaskId, JobId};
use crate::state::AppState;
use crate::tests::{call, count, state, user_id, user_with_team};

// --- a fake agent -------------------------------------------------------------------------------

/// What the fake agent does with one `POST /v1/cron/run`.
#[derive(Clone)]
enum Script {
	/// Prints the lines, then exits with the code.
	Exit(i64, Vec<&'static str>),
	/// Refuses to run it.
	Fail(&'static str),
	/// Never answers.
	Hang,
}

#[derive(Default)]
struct FakeAgent {
	scripts: Mutex<VecDeque<Script>>,
	requests: Mutex<Vec<Value>>,
}

impl FakeAgent {
	fn script(&self, script: Script) {
		self.scripts.lock().unwrap().push_back(script);
	}

	fn requests(&self) -> Vec<Value> {
		self.requests.lock().unwrap().clone()
	}
}

fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	let handler = agent.clone();
	let app = Router::new().route(
		"/v1/cron/run",
		post(move |body: Bytes| async move {
			handler
				.requests
				.lock()
				.unwrap()
				.push(serde_json::from_slice(&body).unwrap());
			let script = handler
				.scripts
				.lock()
				.unwrap()
				.pop_front()
				.unwrap_or(Script::Exit(0, Vec::new()));
			let events: Vec<Value> = match script {
				Script::Hang => {
					return (
						[(header::CONTENT_TYPE, "application/x-ndjson")],
						Body::from_stream(stream::pending::<Result<Bytes, std::io::Error>>()),
					)
						.into_response();
				}
				Script::Exit(code, lines) => lines
					.into_iter()
					.map(|l| json!({ "type": "log", "stream": "stdout", "line": l }))
					.chain([json!({ "type": "exited", "code": code })])
					.collect(),
				Script::Fail(message) => vec![json!({ "type": "failed", "message": message })],
			};
			let body = events.iter().fold(String::new(), |mut out, e| {
				out.push_str(&e.to_string());
				out.push('\n');
				out
			});
			([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
		}),
	);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	agent
}

// --- the fixture --------------------------------------------------------------------------------

struct Fx {
	router: Router,
	state: AppState,
	db: PgPool,
	agent: Arc<FakeAgent>,
	token: String,
	team: i64,
	project: i64,
	server: i64,
	env: i64,
	application: i64,
}

impl Fx {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let agent = fake_agent(&state);
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
		let (s, project) = call(
			&router,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "{project}");
		let project = project["id"].as_i64().unwrap();
		let (s, application) = call(
			&router,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(&token),
			Some(json!({
				"name": "shop", "slug": "shop", "server_id": server,
				"repo_url": "https://github.com/acme/shop", "git_ref": "main",
				"container_port": 8080,
			})),
		)
		.await;
		assert!(s.is_success(), "{application}");
		let application = application["id"].as_i64().unwrap();
		let env = crate::tests::scalar(
			&db,
			"SELECT environment_id FROM applications WHERE id = $1",
			Some(application),
		)
		.await;
		Self {
			router,
			state,
			db,
			agent,
			token,
			team,
			project,
			server,
			env,
			application,
		}
	}

	fn base(&self) -> String {
		format!("/api/v1/teams/{}/projects/{}/cron", self.team, self.project)
	}

	async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.router, method, path, Some(&self.token), body).await
	}

	fn exec_body(&self, name: &str) -> Value {
		json!({
			"environment_id": self.env, "name": name, "kind": "exec",
			"application_id": self.application, "command": "php artisan schedule:run",
			"schedule": "* * * * *",
		})
	}

	fn container_body(&self, name: &str) -> Value {
		json!({
			"environment_id": self.env, "name": name, "kind": "container",
			"server_id": self.server, "image": "docker.io/library/busybox",
			"command": "date", "schedule": "* * * * *",
		})
	}

	async fn create(&self, body: Value) -> i64 {
		let (s, task) = self.call("POST", &self.base(), Some(body)).await;
		assert_eq!(s, StatusCode::CREATED, "{task}");
		task["id"].as_i64().unwrap()
	}

	/// The application is running in a container, so an exec has somewhere to go.
	async fn deploy(&self) {
		sqlx::query(
			"INSERT INTO deployments (id, application_id, status, container_id, image_tag)
			VALUES ($1, $2, 'running', 'shop-container', 'shop:1')",
		)
		.bind(crypto::generate_id())
		.bind(self.application)
		.execute(&self.db)
		.await
		.unwrap();
	}

	async fn set_policy(&self, task: i64, policy: &str) {
		sqlx::query("UPDATE cron_tasks SET concurrency = $2 WHERE id = $1")
			.bind(task)
			.bind(policy)
			.execute(&self.db)
			.await
			.unwrap();
	}

	async fn make_due(&self, task: i64, ago: time::Duration) {
		sqlx::query("UPDATE cron_tasks SET next_run_at = $2 WHERE id = $1")
			.bind(task)
			.bind(OffsetDateTime::now_utc() - ago)
			.execute(&self.db)
			.await
			.unwrap();
	}

	async fn runs(&self, task: i64) -> Vec<super::CronRun> {
		store::list_runs(&self.db, CronTaskId(task), None, 100)
			.await
			.unwrap()
	}

	async fn events(&self) -> Vec<String> {
		sqlx::query_scalar("SELECT kind FROM notification_events ORDER BY id")
			.fetch_all(&self.db)
			.await
			.unwrap()
	}

	/// A channel and a rule that listen to every cron event, so the quiet ones (a recovery is
	/// only "info") are kept too.
	async fn listen(&self) {
		let (channel, rule) = (crypto::generate_id(), crypto::generate_id());
		sqlx::query(
			"INSERT INTO notification_channels (id, team_id, name, kind, config)
			VALUES ($1, $2, 'hook', 'webhook', '{\"url\": \"http://127.0.0.1:1/hook\"}')",
		)
		.bind(channel)
		.bind(self.team)
		.execute(&self.db)
		.await
		.unwrap();
		sqlx::query(
			"INSERT INTO notification_rules (id, team_id, name, kinds)
			VALUES ($1, $2, 'cron', ARRAY['cron_failed', 'cron_recovered'])",
		)
		.bind(rule)
		.bind(self.team)
		.execute(&self.db)
		.await
		.unwrap();
		sqlx::query("INSERT INTO notification_rule_channels (rule_id, channel_id) VALUES ($1, $2)")
			.bind(rule)
			.bind(channel)
			.execute(&self.db)
			.await
			.unwrap();
	}

	/// Starts the job dispatcher; the returned token stops it.
	fn dispatch(&self) -> CancellationToken {
		let stop = CancellationToken::new();
		tokio::spawn(crate::jobs::worker::run_dispatcher(
			self.state.clone(),
			stop.clone(),
		));
		stop
	}

	/// Waits until the newest run of `task` satisfies `done`.
	async fn wait_for(
		&self,
		task: i64,
		what: &str,
		done: impl Fn(&[super::CronRun]) -> bool,
	) -> Vec<super::CronRun> {
		for _ in 0..600 {
			let runs = self.runs(task).await;
			if done(&runs) {
				return runs;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
		panic!("timed out waiting for {what}: {:?}", self.runs(task).await);
	}
}

// --- the API ------------------------------------------------------------------------------------

#[sqlx::test]
async fn cron_a_task_is_created_listed_and_validated(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	let id2 = fx.create(fx.container_body("date")).await;

	let (s, list) = fx.call("GET", &fx.base(), None).await;
	assert_eq!(s, StatusCode::OK);
	let list = list.as_array().unwrap();
	assert_eq!(list.len(), 2);
	let laravel = list.iter().find(|t| t["id"] == id).unwrap();
	assert_eq!(laravel["application"], "shop");
	assert_eq!(laravel["timezone"], "UTC");
	assert_eq!(laravel["concurrency"], "skip");
	assert!(laravel["next_run_at"].is_string(), "{laravel}");
	assert!(list.iter().any(|t| t["id"] == id2));

	let bad: Vec<(&str, Value)> = vec![
		("schedule", json!({ "schedule": "every day" })),
		("seconds field", json!({ "schedule": "0 0 3 * * *" })),
		("zone", json!({ "timezone": "Mars/Olympus" })),
		("timeout", json!({ "timeout_s": 0 })),
		("long timeout", json!({ "timeout_s": 90_000 })),
		("retries", json!({ "max_retries": 11 })),
		("backoff", json!({ "retry_backoff_s": 0 })),
		("command", json!({ "command": "   " })),
		("name", json!({ "name": "" })),
	];
	for (what, patch) in bad {
		let mut body = fx.exec_body(&format!("bad-{what}"));
		for (k, v) in patch.as_object().unwrap() {
			body[k] = v.clone();
		}
		let (s, _) = fx.call("POST", &fx.base(), Some(body)).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{what}");
	}
	// An exec needs an application; a container needs an image or one.
	let mut no_app = fx.exec_body("no-app");
	no_app["application_id"] = Value::Null;
	assert_eq!(
		fx.call("POST", &fx.base(), Some(no_app)).await.0,
		StatusCode::BAD_REQUEST
	);
	let mut no_image = fx.container_body("no-image");
	no_image["image"] = Value::Null;
	assert_eq!(
		fx.call("POST", &fx.base(), Some(no_image)).await.0,
		StatusCode::BAD_REQUEST
	);
	// Names are unique in an environment.
	let (s, _) = fx
		.call("POST", &fx.base(), Some(fx.exec_body("laravel")))
		.await;
	assert_eq!(s, StatusCode::CONFLICT);
	assert_eq!(count(&fx.db, "cron_tasks").await, 2);
}

#[sqlx::test]
async fn cron_update_pause_resume_and_delete(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	let path = format!("{}/{id}", fx.base());

	let (s, task) = fx
		.call(
			"PATCH",
			&path,
			Some(json!({ "schedule": "0 3 * * *", "timezone": "Europe/Berlin", "max_retries": 2 })),
		)
		.await;
	assert_eq!(s, StatusCode::OK, "{task}");
	assert_eq!(task["timezone"], "Europe/Berlin");
	assert_eq!(task["max_retries"], 2);
	let next = task["next_run_at"].as_str().unwrap().to_owned();
	assert!(
		next.contains("T01:00:00") || next.contains("T02:00:00"),
		"{next}"
	);

	let (_, paused) = fx.call("POST", &format!("{path}/pause"), None).await;
	assert_eq!(paused["enabled"], false);
	assert!(paused["next_run_at"].is_null());
	let (_, resumed) = fx.call("POST", &format!("{path}/resume"), None).await;
	assert_eq!(resumed["enabled"], true);
	assert!(resumed["next_run_at"].is_string());

	// A bad edit changes nothing.
	let (s, _) = fx
		.call("PATCH", &path, Some(json!({ "schedule": "nope" })))
		.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
	let (_, same) = fx.call("GET", &path, None).await;
	assert_eq!(same["schedule"], "0 3 * * *");

	let (s, _) = fx.call("DELETE", &path, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(fx.call("GET", &path, None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn cron_another_teams_tasks_are_404(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	let run = fx
		.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await
		.1["id"]
		.as_i64()
		.unwrap();

	let (bob, bob_team) = user_with_team(&fx.router, "bob").await;
	// Bob's own team, Alice's project and task ids.
	let base = format!("/api/v1/teams/{bob_team}/projects/{}/cron", fx.project);
	let task = format!("{base}/{id}");
	for (method, path, body) in [
		("GET", task.clone(), None),
		("PATCH", task.clone(), Some(json!({ "name": "mine" }))),
		("DELETE", task.clone(), None),
		("POST", format!("{task}/run"), None),
		("POST", format!("{task}/pause"), None),
		("POST", format!("{task}/resume"), None),
		("GET", format!("{task}/runs"), None),
		("GET", format!("{task}/runs/{run}"), None),
		("POST", format!("{task}/runs/{run}/cancel"), None),
		("GET", base.clone(), None),
		("POST", base.clone(), Some(fx.exec_body("stolen"))),
	] {
		let (s, _) = call(&fx.router, method, &path, Some(&bob), body).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	// A task id of Bob's own project does not reach Alice's task either.
	let (_, project) = call(
		&fx.router,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	let mine = project["id"].as_i64().unwrap();
	let (s, _) = call(
		&fx.router,
		"GET",
		&format!("/api/v1/teams/{bob_team}/projects/{mine}/cron/{id}"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// Alice's run of someone else's task id in her own project is a 404 too.
	let (s, _) = fx
		.call("GET", &format!("{}/{}/runs/{run}", fx.base(), id + 1), None)
		.await;
	assert_eq!(s, StatusCode::NOT_FOUND);

	assert_eq!(count(&fx.db, "cron_tasks").await, 1);
	assert_eq!(count(&fx.db, "cron_runs").await, 1);
	assert_eq!(
		fx.call("GET", &format!("{}/{id}", fx.base()), None).await.0,
		StatusCode::OK
	);
}

#[sqlx::test]
async fn cron_a_viewer_cannot_change_anything(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	let (carol, _) = user_with_team(&fx.router, "carol").await;
	let carol_id = user_id(&fx.db, "carol").await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(fx.team)
		.bind(carol_id)
		.execute(&fx.db)
		.await
		.unwrap();

	let task = format!("{}/{id}", fx.base());
	let (s, list) = call(&fx.router, "GET", &fx.base(), Some(&carol), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);
	for (method, path, body) in [
		("POST", fx.base(), Some(fx.exec_body("forged"))),
		(
			"PATCH",
			task.clone(),
			Some(json!({ "command": "rm -rf /" })),
		),
		("DELETE", task.clone(), None),
		("POST", format!("{task}/run"), None),
		("POST", format!("{task}/pause"), None),
	] {
		let (s, _) = call(&fx.router, method, &path, Some(&carol), body).await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}");
	}
	assert_eq!(count(&fx.db, "cron_tasks").await, 1);
	assert_eq!(count(&fx.db, "cron_runs").await, 0);
	let (_, same) = fx.call("GET", &task, None).await;
	assert_eq!(same["command"], "php artisan schedule:run");
}

#[sqlx::test]
async fn cron_preview_lists_the_next_five_runs(db: PgPool) {
	let fx = Fx::new(db).await;
	let (s, p) = fx
		.call(
			"GET",
			&format!(
				"{}/preview?schedule=*/10%20*%20*%20*%20*&timezone=Asia/Kolkata",
				fx.base()
			),
			None,
		)
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(p["runs"].as_array().unwrap().len(), 5);
	assert!(p["error"].is_null());
	let (_, p) = fx
		.call("GET", &format!("{}/preview?schedule=nope", fx.base()), None)
		.await;
	assert!(p["error"].is_string());
	assert!(p["runs"].as_array().unwrap().is_empty());
}

// --- the scheduler ------------------------------------------------------------------------------

#[sqlx::test]
async fn cron_a_due_task_gets_one_run_and_moves_on(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	fx.make_due(id, time::Duration::seconds(5)).await;
	let now = OffsetDateTime::now_utc();

	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 1);
	let runs = fx.runs(id).await;
	assert_eq!(runs.len(), 1);
	assert_eq!(runs[0].status, RunStatus::Queued);
	assert_eq!(runs[0].trigger, Trigger::Schedule);
	assert_eq!(runs[0].attempt, 1);
	let (kind, lock): (String, String) =
		sqlx::query_as("SELECT kind, lock_key FROM jobs WHERE id = $1")
			.bind(runs[0].job_id.unwrap())
			.fetch_one(&fx.db)
			.await
			.unwrap();
	assert_eq!(kind, "cron_run");
	assert_eq!(lock, format!("cron:{id}"));

	let task = store::task_by_id(&fx.db, CronTaskId(id))
		.await
		.unwrap()
		.unwrap();
	assert!(task.next_run_at.unwrap() > now);
	// The slot is taken: sweeping again changes nothing.
	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 0);
	assert_eq!(fx.runs(id).await.len(), 1);
	// A paused task never runs.
	fx.call("POST", &format!("{}/{id}/pause", fx.base()), None)
		.await;
	fx.make_due(id, time::Duration::seconds(5)).await;
	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 0);
}

#[sqlx::test]
async fn cron_concurrency_skip_records_a_skipped_run(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	let now = OffsetDateTime::now_utc();
	fx.make_due(id, time::Duration::seconds(5)).await;
	scheduler::sweep(&fx.state, now).await.unwrap();
	fx.make_due(id, time::Duration::seconds(5)).await;
	scheduler::sweep(&fx.state, now).await.unwrap();

	let runs = fx.runs(id).await;
	assert_eq!(runs.len(), 2);
	assert_eq!(runs[0].status, RunStatus::Skipped);
	assert!(runs[0].job_id.is_none());
	assert!(runs[0].error.as_deref().unwrap().contains("previous run"));
	assert_eq!(runs[1].status, RunStatus::Queued);
	// Run now says no too, for a skip task that is busy.
	let (s, _) = fx
		.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	assert_eq!(s, StatusCode::CONFLICT);

	// Once the first run is over the next one is queued again.
	sqlx::query("UPDATE cron_runs SET status = 'succeeded', finished_at = NOW() WHERE id = $1")
		.bind(runs[1].id)
		.execute(&fx.db)
		.await
		.unwrap();
	fx.make_due(id, time::Duration::seconds(5)).await;
	scheduler::sweep(&fx.state, now).await.unwrap();
	assert_eq!(fx.runs(id).await[0].status, RunStatus::Queued);
}

#[sqlx::test]
async fn cron_concurrency_queue_waits_and_allow_overlaps(db: PgPool) {
	let fx = Fx::new(db).await;
	let queue = fx.create(fx.exec_body("queued")).await;
	let allow = fx.create(fx.exec_body("allowed")).await;
	fx.set_policy(queue, "queue").await;
	fx.set_policy(allow, "allow").await;
	let now = OffsetDateTime::now_utc();

	for _ in 0..7 {
		fx.make_due(queue, time::Duration::seconds(5)).await;
		fx.make_due(allow, time::Duration::seconds(5)).await;
		scheduler::sweep(&fx.state, now).await.unwrap();
	}
	// The queue holds five, and the other two are said to be skipped.
	let runs = fx.runs(queue).await;
	assert_eq!(
		runs.iter()
			.filter(|r| r.status == RunStatus::Queued)
			.count(),
		5
	);
	assert_eq!(
		runs.iter()
			.filter(|r| r.status == RunStatus::Skipped)
			.count(),
		2
	);
	let locks: Vec<String> =
		sqlx::query_scalar("SELECT DISTINCT lock_key FROM jobs WHERE target_id = ANY($1)")
			.bind(runs.iter().map(|r| r.id.0).collect::<Vec<_>>())
			.fetch_all(&fx.db)
			.await
			.unwrap();
	assert_eq!(
		locks,
		[format!("cron:{queue}")],
		"one lock key: they run one at a time"
	);

	// `allow` runs all seven, each with a lock key of its own.
	let runs = fx.runs(allow).await;
	assert_eq!(
		runs.iter()
			.filter(|r| r.status == RunStatus::Queued)
			.count(),
		7
	);
	let locks: Vec<String> =
		sqlx::query_scalar("SELECT DISTINCT lock_key FROM jobs WHERE target_id = ANY($1)")
			.bind(runs.iter().map(|r| r.id.0).collect::<Vec<_>>())
			.fetch_all(&fx.db)
			.await
			.unwrap();
	assert_eq!(locks.len(), 7);
}

#[sqlx::test]
async fn cron_a_queued_run_is_held_back_by_the_running_one(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("queued")).await;
	fx.set_policy(id, "queue").await;
	let now = OffsetDateTime::now_utc();
	for _ in 0..2 {
		fx.make_due(id, time::Duration::seconds(5)).await;
		scheduler::sweep(&fx.state, now).await.unwrap();
	}
	// Claim like the dispatcher: the second run's job cannot start while the first runs.
	let claim = |db: &PgPool| {
		let db = db.clone();
		async move {
			sqlx::query_scalar::<_, i64>(
				"UPDATE jobs SET status = 'running' WHERE id = (
					SELECT j.id FROM jobs j WHERE j.status = 'queued' AND j.run_after <= NOW()
						AND NOT EXISTS (SELECT 1 FROM jobs r WHERE r.status = 'running' AND r.lock_key = j.lock_key)
					ORDER BY j.id LIMIT 1) RETURNING id",
			)
			.fetch_optional(&db)
			.await
			.unwrap()
		}
	};
	assert!(claim(&fx.db).await.is_some());
	assert!(claim(&fx.db).await.is_none());
}

#[sqlx::test]
async fn cron_catch_up_runs_once_after_downtime(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	// Every minute, and podkit was down for three hours: 180 slots missed.
	fx.make_due(id, time::Duration::hours(3)).await;
	let now = OffsetDateTime::now_utc();
	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 1);
	let runs = fx.runs(id).await;
	assert_eq!(runs.len(), 1, "one run, not 180");
	assert_eq!(runs[0].trigger, Trigger::CatchUp);
	let task = store::task_by_id(&fx.db, CronTaskId(id))
		.await
		.unwrap()
		.unwrap();
	assert!(
		task.next_run_at.unwrap() > now,
		"and then on schedule again"
	);
	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 0);

	// A slot that is only a little late is an ordinary run.
	let other = fx.create(fx.exec_body("late")).await;
	fx.make_due(other, time::Duration::seconds(30)).await;
	scheduler::sweep(&fx.state, OffsetDateTime::now_utc())
		.await
		.unwrap();
	assert_eq!(fx.runs(other).await[0].trigger, Trigger::Schedule);
}

#[sqlx::test]
async fn cron_only_the_leader_sweeps(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	fx.make_due(id, time::Duration::seconds(5)).await;
	let now = OffsetDateTime::now_utc();

	// Another process holds the leader lock: this one does nothing.
	let mut other = fx.db.begin().await.unwrap();
	let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
		.bind(0x706f_646b_6974_0001_i64)
		.fetch_one(&mut *other)
		.await
		.unwrap();
	assert!(got);
	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 0);
	assert!(fx.runs(id).await.is_empty());
	// It leaves, and the lock goes with its transaction.
	other.rollback().await.unwrap();
	assert_eq!(scheduler::sweep(&fx.state, now).await.unwrap(), 1);

	// Two sweeps at once queue the slot once.
	let again = fx.create(fx.exec_body("again")).await;
	fx.make_due(again, time::Duration::seconds(5)).await;
	let (a, b) = tokio::join!(
		scheduler::sweep(&fx.state, now),
		scheduler::sweep(&fx.state, now)
	);
	assert_eq!(a.unwrap() + b.unwrap(), 1);
	assert_eq!(fx.runs(again).await.len(), 1);
}

#[test]
fn cron_retries_wait_longer_each_time() {
	assert_eq!(run::retry_delay(60, 1), Duration::from_secs(60));
	assert_eq!(run::retry_delay(60, 2), Duration::from_secs(120));
	assert_eq!(run::retry_delay(60, 3), Duration::from_secs(240));
	// There is a ceiling.
	assert_eq!(run::retry_delay(3600, 30), Duration::from_hours(6));
}

#[sqlx::test]
async fn cron_old_runs_and_their_logs_are_purged(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	let now = OffsetDateTime::now_utc();
	let mut keep = Vec::new();
	for age in [40, 31, 2, 1] {
		let run = CronRunId::generate();
		let job = JobId::generate();
		sqlx::query(
			"INSERT INTO jobs (id, kind, server_id, target_id, lock_key, status, finished_at)
			VALUES ($1, 'cron_run', $2, $3, 'x', 'succeeded', $4)",
		)
		.bind(job)
		.bind(fx.server)
		.bind(run)
		.bind(now - time::Duration::days(age))
		.execute(&fx.db)
		.await
		.unwrap();
		sqlx::query(
			"INSERT INTO cron_runs (id, task_id, job_id, status, trigger, finished_at)
			VALUES ($1, $2, $3, 'succeeded', 'schedule', $4)",
		)
		.bind(run)
		.bind(id)
		.bind(job)
		.bind(now - time::Duration::days(age))
		.execute(&fx.db)
		.await
		.unwrap();
		std::fs::write(fx.state.logs.path(job), "{}\n").unwrap();
		if age < 30 {
			keep.push(job);
		}
	}
	scheduler::purge(&fx.state).await;
	assert_eq!(
		fx.runs(id).await.len(),
		2,
		"the two older than 30 days are gone"
	);
	for job in keep {
		assert!(fx.state.logs.path(job).exists());
	}
	assert_eq!(count(&fx.db, "jobs").await, 2);
}

// --- driving the agent's stream -----------------------------------------------------------------

/// A stream that never yields and says when it is dropped: the connection closing.
struct Watch(Arc<AtomicBool>);

impl futures_util::Stream for Watch {
	type Item = Result<Event, ServerError>;

	fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		Poll::Pending
	}
}

impl Drop for Watch {
	fn drop(&mut self) {
		self.0.store(true, Ordering::SeqCst);
	}
}

fn lines(items: Vec<Result<Event, ServerError>>) -> BoxStream<'static, Result<Event, ServerError>> {
	stream::iter(items).boxed()
}

fn out(line: &str) -> Event {
	Event::Log {
		stream: LogStream::Stdout,
		line: line.to_owned(),
	}
}

fn log_text(state: &AppState, job: JobId) -> String {
	std::fs::read_to_string(state.logs.path(job)).unwrap()
}

#[sqlx::test]
async fn cron_a_timeout_stops_the_run_and_closes_the_connection(db: PgPool) {
	let state = state(db);
	let mut log = state.logs.open(JobId(1)).await.unwrap();
	let dropped = Arc::new(AtomicBool::new(false));
	let started = std::time::Instant::now();
	let outcome = run::drive(
		Box::pin(Watch(dropped.clone())),
		&mut log,
		&CancellationToken::new(),
		Duration::from_millis(100),
	)
	.await;
	assert_eq!(outcome, Outcome::TimedOut);
	assert!(started.elapsed() < Duration::from_secs(5));
	// The stream is gone: that is what makes the agent kill the command.
	assert!(dropped.load(Ordering::SeqCst));
	log.finish().await;
	assert!(log_text(&state, JobId(1)).contains("timed out after"));
}

#[sqlx::test]
async fn cron_cancel_stops_the_run_too(db: PgPool) {
	let state = state(db);
	let mut log = state.logs.open(JobId(2)).await.unwrap();
	let dropped = Arc::new(AtomicBool::new(false));
	let cancel = CancellationToken::new();
	let stop = cancel.clone();
	tokio::spawn(async move {
		tokio::time::sleep(Duration::from_millis(50)).await;
		stop.cancel();
	});
	let outcome = run::drive(
		Box::pin(Watch(dropped.clone())),
		&mut log,
		&cancel,
		Duration::from_secs(30),
	)
	.await;
	assert_eq!(outcome, Outcome::Cancelled);
	assert!(dropped.load(Ordering::SeqCst));
}

#[sqlx::test]
async fn cron_the_outcome_follows_the_agents_last_word(db: PgPool) {
	let state = state(db);
	let mut log = state.logs.open(JobId(3)).await.unwrap();
	let go = |events: Vec<Result<Event, ServerError>>| lines(events);
	let outcome = run::drive(
		go(vec![
			Ok(out("a")),
			Ok(out("b")),
			Ok(Event::Exited { code: 3 }),
		]),
		&mut log,
		&CancellationToken::new(),
		Duration::from_secs(5),
	)
	.await;
	assert_eq!(outcome, Outcome::Exited(3));
	let outcome = run::drive(
		go(vec![Ok(Event::Failed {
			message: "the application's container is not running".into(),
		})]),
		&mut log,
		&CancellationToken::new(),
		Duration::from_secs(5),
	)
	.await;
	assert_eq!(
		outcome,
		Outcome::Failed("the application's container is not running".into())
	);
	// A stream that ends with no word means the connection was lost.
	let outcome = run::drive(
		go(vec![Ok(out("a"))]),
		&mut log,
		&CancellationToken::new(),
		Duration::from_secs(5),
	)
	.await;
	assert!(matches!(outcome, Outcome::Failed(m) if m.contains("lost connection")));
	log.finish().await;
	let text = log_text(&state, JobId(3));
	assert!(
		text.contains("\"line\":\"a\"") && text.contains("\"line\":\"b\""),
		"{text}"
	);
}

#[sqlx::test]
async fn cron_a_log_is_cut_at_one_mebibyte(db: PgPool) {
	let state = state(db);
	let mut log = state.logs.open(JobId(4)).await.unwrap();
	let line = "x".repeat(4000);
	let mut events: Vec<Result<Event, ServerError>> = (0..400).map(|_| Ok(out(&line))).collect();
	events.push(Ok(Event::Exited { code: 0 }));
	let outcome = run::drive(
		lines(events),
		&mut log,
		&CancellationToken::new(),
		Duration::from_secs(10),
	)
	.await;
	assert_eq!(outcome, Outcome::Exited(0), "the command still ends well");
	log.finish().await;
	let text = log_text(&state, JobId(4));
	assert!(text.contains("log truncated at 1 MiB"));
	// 1 MiB of text plus the JSON around each line, and not the 1.6 MB that was printed.
	assert!(text.len() < 1_400_000, "{}", text.len());
	assert!(text.len() > 1_000_000, "{}", text.len());
}

// --- whole runs through a fake agent ------------------------------------------------------------

#[sqlx::test]
async fn cron_run_now_runs_an_exec_and_keeps_its_log(db: PgPool) {
	let fx = Fx::new(db).await;
	fx.deploy().await;
	let stop = fx.dispatch();
	let id = fx.create(fx.exec_body("laravel")).await;
	fx.agent
		.script(Script::Exit(0, vec!["Running scheduled tasks", "done"]));

	let (s, run) = fx
		.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(run["trigger"], "manual");
	let runs = fx
		.wait_for(id, "the run to succeed", |r| {
			r.first().is_some_and(|r| r.status == RunStatus::Succeeded)
		})
		.await;
	assert_eq!(runs[0].exit_code, Some(0));
	assert!(runs[0].started_at.is_some() && runs[0].finished_at.is_some());

	let sent = fx.agent.requests();
	assert_eq!(sent.len(), 1);
	assert_eq!(sent[0]["target"]["type"], "exec");
	assert_eq!(sent[0]["target"]["container"], "shop-container");
	assert_eq!(sent[0]["target"]["command"], "php artisan schedule:run");
	assert_eq!(sent[0]["run_id"], runs[0].id.0);

	let log = log_text(&fx.state, runs[0].job_id.unwrap());
	assert!(log.contains("Running scheduled tasks"), "{log}");
	assert!(fx.events().await.is_empty(), "no news is good news");
	stop.cancel();
}

#[sqlx::test]
async fn cron_a_container_task_sends_the_image_networks_and_no_aliases(db: PgPool) {
	let fx = Fx::new(db).await;
	fx.deploy().await;
	let stop = fx.dispatch();
	let id = fx.create(fx.container_body("date")).await;
	let task = fx
		.create(json!({
			"environment_id": fx.env, "name": "report", "kind": "container",
			"application_id": fx.application, "command": "php artisan report", "schedule": "0 6 * * *",
		}))
		.await;
	sqlx::query(
		"INSERT INTO env_vars (id, application_id, key, value) VALUES ($1, $2, 'APP_KEY', $3)",
	)
	.bind(crypto::generate_id())
	.bind(fx.application)
	.bind(fx.state.secrets.encrypt("s3cret").unwrap())
	.execute(&fx.db)
	.await
	.unwrap();

	for task in [id, task] {
		fx.call("POST", &format!("{}/{task}/run", fx.base()), None)
			.await;
		fx.wait_for(task, "a finished run", |r| {
			r.first().is_some_and(|r| !r.status.is_active())
		})
		.await;
	}
	let sent = fx.agent.requests();
	assert_eq!(sent.len(), 2);
	let by_image: Vec<&Value> = sent.iter().collect();
	let busybox = by_image
		.iter()
		.find(|r| r["target"]["image"] == "docker.io/library/busybox")
		.unwrap();
	assert_eq!(busybox["target"]["command"], "date");
	assert!(busybox["target"]["env"].is_null());
	assert!(
		busybox["target"]["attach"][0]["name"]
			.as_str()
			.unwrap()
			.starts_with("podkit-env-")
	);
	assert!(busybox["target"]["attach"][0]["aliases"].is_null());

	// From an application it runs the app's current image with the app's variables.
	let report = by_image
		.iter()
		.find(|r| r["target"]["command"] == "php artisan report")
		.unwrap();
	assert_eq!(report["target"]["image"], "shop:1");
	assert_eq!(report["target"]["env"], json!([["APP_KEY", "s3cret"]]));
	assert!(
		report["target"]["attach"][0]["aliases"].is_null(),
		"{report}"
	);
	stop.cancel();
}

#[sqlx::test]
async fn cron_a_failed_run_says_so_once_and_recovery_is_announced(db: PgPool) {
	let fx = Fx::new(db).await;
	fx.deploy().await;
	fx.listen().await;
	let stop = fx.dispatch();
	let id = fx.create(fx.exec_body("laravel")).await;

	fx.agent.script(Script::Exit(3, vec!["boom"]));
	// The mutating call goes first: the task is failing.
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	let runs = fx
		.wait_for(id, "the failure", |r| {
			r.first().is_some_and(|r| r.status == RunStatus::Failed)
		})
		.await;
	assert_eq!(runs[0].exit_code, Some(3));
	assert_eq!(runs[0].error.as_deref(), Some("exited with code 3"));
	for _ in 0..100 {
		if !fx.events().await.is_empty() {
			break;
		}
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
	assert_eq!(fx.events().await, ["cron_failed"]);
	let (title, body, link): (String, String, Option<String>) =
		sqlx::query_as("SELECT title, body, link FROM notification_events")
			.fetch_one(&fx.db)
			.await
			.unwrap();
	assert_eq!(title, "laravel failed");
	assert_eq!(body, "exited with code 3");
	assert_eq!(
		link.unwrap(),
		format!("/teams/{}/projects/{}/cron/{id}", fx.team, fx.project)
	);

	// It runs well the next time: one recovery event.
	fx.agent.script(Script::Exit(0, vec![]));
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	fx.wait_for(id, "the success", |r| {
		r.first().is_some_and(|r| r.status == RunStatus::Succeeded)
	})
	.await;
	for _ in 0..100 {
		if fx.events().await.len() == 2 {
			break;
		}
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
	assert_eq!(fx.events().await, ["cron_failed", "cron_recovered"]);
	// Another success is not news.
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	fx.wait_for(id, "a third run", |r| {
		r.len() == 3 && !r[0].status.is_active()
	})
	.await;
	tokio::time::sleep(Duration::from_millis(200)).await;
	assert_eq!(fx.events().await.len(), 2);
	stop.cancel();
}

#[sqlx::test]
async fn cron_a_failure_is_retried_with_backoff_before_anyone_is_told(db: PgPool) {
	let fx = Fx::new(db).await;
	fx.deploy().await;
	let stop = fx.dispatch();
	let id = fx.create(fx.exec_body("laravel")).await;
	fx.call(
		"PATCH",
		&format!("{}/{id}", fx.base()),
		Some(json!({ "max_retries": 2, "retry_backoff_s": 1 })),
	)
	.await;
	fx.agent
		.script(Script::Fail("the application's container is not running"));
	fx.agent
		.script(Script::Fail("the application's container is not running"));
	fx.agent.script(Script::Exit(0, vec!["finally"]));

	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	let runs = fx
		.wait_for(id, "three attempts", |r| {
			r.len() == 3 && r.iter().all(|r| !r.status.is_active())
		})
		.await;
	let mut by_attempt: Vec<_> = runs
		.iter()
		.map(|r| (r.attempt, r.status, r.trigger))
		.collect();
	by_attempt.sort_by_key(|(attempt, ..)| *attempt);
	assert_eq!(
		by_attempt,
		[
			(1, RunStatus::Failed, Trigger::Manual),
			(2, RunStatus::Failed, Trigger::Retry),
			(3, RunStatus::Succeeded, Trigger::Retry),
		]
	);
	// The second attempt waited the backoff (1 s) and the third twice that.
	let mut started: Vec<_> = runs
		.iter()
		.map(|r| (r.attempt, r.started_at.unwrap()))
		.collect();
	started.sort_by_key(|(attempt, _)| *attempt);
	assert!(started[1].1 - started[0].1 >= time::Duration::milliseconds(900));
	assert!(started[2].1 - started[1].1 >= time::Duration::milliseconds(1900));
	// It got there in the end, so nobody was told.
	assert!(fx.events().await.is_empty());

	// With the retries used up, the last failure is the news.
	for _ in 0..3 {
		fx.agent.script(Script::Fail("nope"));
	}
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	fx.wait_for(id, "three more attempts", |r| {
		r.len() == 6 && r.iter().all(|r| !r.status.is_active())
	})
	.await;
	for _ in 0..100 {
		if !fx.events().await.is_empty() {
			break;
		}
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
	assert_eq!(fx.events().await, ["cron_failed"]);
	let title: String = sqlx::query_scalar("SELECT title FROM notification_events")
		.fetch_one(&fx.db)
		.await
		.unwrap();
	assert_eq!(title, "laravel failed after 3 attempts");
	stop.cancel();
}

#[sqlx::test]
async fn cron_a_run_that_hangs_times_out_and_can_be_cancelled(db: PgPool) {
	let fx = Fx::new(db).await;
	fx.deploy().await;
	let stop = fx.dispatch();
	let id = fx.create(fx.exec_body("slow")).await;
	fx.call(
		"PATCH",
		&format!("{}/{id}", fx.base()),
		Some(json!({ "timeout_s": 1 })),
	)
	.await;

	fx.agent.script(Script::Hang);
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	let runs = fx
		.wait_for(id, "the timeout", |r| {
			r.first().is_some_and(|r| r.status == RunStatus::Failed)
		})
		.await;
	assert_eq!(runs[0].error.as_deref(), Some("timed out after 1 s"));

	// Without a timeout to save it, a hung run is cancelled by hand.
	fx.call(
		"PATCH",
		&format!("{}/{id}", fx.base()),
		Some(json!({ "timeout_s": 600 })),
	)
	.await;
	fx.agent.script(Script::Hang);
	let (_, run) = fx
		.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	let run_id = run["id"].as_i64().unwrap();
	fx.wait_for(id, "it to start", |r| {
		r.first().is_some_and(|r| r.status == RunStatus::Running)
	})
	.await;
	let (s, _) = fx
		.call(
			"POST",
			&format!("{}/{id}/runs/{run_id}/cancel", fx.base()),
			None,
		)
		.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	let runs = fx
		.wait_for(id, "the cancel", |r| {
			r.first().is_some_and(|r| r.status == RunStatus::Cancelled)
		})
		.await;
	assert_eq!(runs[0].id.0, run_id);
	// A cancel is the user's doing: no event beyond the timeout's. And a finished run cannot
	// be cancelled again.
	assert_eq!(fx.events().await, ["cron_failed"]);
	let (s, _) = fx
		.call(
			"POST",
			&format!("{}/{id}/runs/{run_id}/cancel", fx.base()),
			None,
		)
		.await;
	assert_eq!(s, StatusCode::CONFLICT);
	stop.cancel();
}

#[sqlx::test]
async fn cron_a_run_waiting_in_the_queue_can_be_cancelled(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	// No dispatcher: the run stays queued.
	let (_, run) = fx
		.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	let run_id = run["id"].as_i64().unwrap();
	let (s, _) = fx
		.call(
			"POST",
			&format!("{}/{id}/runs/{run_id}/cancel", fx.base()),
			None,
		)
		.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(fx.runs(id).await[0].status, RunStatus::Cancelled);
}

#[sqlx::test]
async fn cron_a_task_whose_app_is_not_running_fails_the_run_with_a_reason(db: PgPool) {
	let fx = Fx::new(db).await;
	let stop = fx.dispatch();
	let id = fx.create(fx.exec_body("laravel")).await;
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	let runs = fx
		.wait_for(id, "the failure", |r| {
			r.first().is_some_and(|r| r.status == RunStatus::Failed)
		})
		.await;
	assert_eq!(
		runs[0].error.as_deref(),
		Some("the application is not running")
	);
	assert!(fx.agent.requests().is_empty(), "the agent was never asked");
	stop.cancel();
}

#[sqlx::test]
async fn cron_deleting_a_task_takes_its_runs_along(db: PgPool) {
	let fx = Fx::new(db).await;
	let id = fx.create(fx.exec_body("laravel")).await;
	fx.call("POST", &format!("{}/{id}/run", fx.base()), None)
		.await;
	assert_eq!(count(&fx.db, "cron_runs").await, 1);
	let (s, _) = fx
		.call("DELETE", &format!("{}/{id}", fx.base()), None)
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&fx.db, "cron_runs").await, 0);
	// Deleting the application takes its tasks.
	let other = fx.create(fx.exec_body("again")).await;
	sqlx::query("DELETE FROM applications WHERE id = $1")
		.bind(fx.application)
		.execute(&fx.db)
		.await
		.unwrap();
	assert!(
		store::task_by_id(&fx.db, CronTaskId(other))
			.await
			.unwrap()
			.is_none()
	);
}

#[test]
fn cron_durations_read_in_one_unit() {
	use super::service::duration;
	assert_eq!(duration(1), "1 s");
	assert_eq!(duration(90), "90 s");
	assert_eq!(duration(300), "5 min");
	assert_eq!(duration(7200), "2 h");
}
