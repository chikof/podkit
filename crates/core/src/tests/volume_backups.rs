//! The fixtures the volume backups tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `volume_backups::tests`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::jobs::{self, Job, JobFailure, JobKind};
use crate::state::AppState;
use crate::tests::{call, insert_deployment, server_id, state, user_with_team};

/// A value that must never come back out, whatever the page.
pub const SECRET: &str = "S3CR3T-volume-backup-0b9a1c5e";

pub const BUCKET: &str = "podkit";

// --- the fakes -----------------------------------------------------------------------------

/// What the fake agent saw, in order, and what it should answer differently.
#[derive(Default)]
pub struct Agent {
	pub calls: Mutex<Vec<String>>,
	pub fail_dump: AtomicBool,
	pub fail_restore: AtomicBool,
}

impl Agent {
	pub fn log(&self, line: impl Into<String>) {
		self.calls.lock().unwrap().push(line.into());
	}

	pub fn calls(&self) -> Vec<String> {
		self.calls.lock().unwrap().clone()
	}

	pub fn clear(&self) {
		self.calls.lock().unwrap().clear();
	}
}

pub fn ndjson(events: &[Value]) -> axum::response::Response {
	let body = events.iter().fold(String::new(), |mut out, e| {
		out.push_str(&e.to_string());
		out.push('\n');
		out
	});
	([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

/// A fake agent on the local server's socket.
pub fn fake_agent(state: &AppState) -> Arc<Agent> {
	let agent = Arc::new(Agent::default());
	let app = Router::new()
		.route(
			"/v1/containers/{id}/stop",
			post(
				|State(a): State<Arc<Agent>>, Path(id): Path<String>| async move {
					a.log(format!("stop {id}"));
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/v1/containers/{id}/start",
			post(
				|State(a): State<Arc<Agent>>, Path(id): Path<String>| async move {
					a.log(format!("start {id}"));
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/v1/volume-backups/dump",
			post(|State(a): State<Arc<Agent>>, body: Bytes| async move {
				let req: Value = serde_json::from_slice(&body).unwrap();
				a.log(format!("dump {}", req["volume"].as_str().unwrap()));
				if a.fail_dump.load(Ordering::SeqCst) {
					return ndjson(&[
						json!({ "type": "failed", "message": "reading the volume: boom" }),
					]);
				}
				ndjson(&[
					json!({ "type": "log", "stream": "system", "line": "archiving read-only" }),
					json!({ "type": "volume_dumped", "path": "/agent/backups/1.tar.gz", "size": 1234,
						"sha256": "ab".repeat(32), "entries": 3, "raw_bytes": 4096 }),
				])
			}),
		)
		.route(
			"/v1/volume-backups/upload",
			post(|State(a): State<Arc<Agent>>, body: Bytes| async move {
				let req: Value = serde_json::from_slice(&body).unwrap();
				let urls = req["urls"].as_array().unwrap().len();
				a.log(format!("upload {urls} part(s) of {}", req["part_size"]));
				ndjson(&[json!({ "type": "parts_uploaded",
					"etags": (1..=urls).map(|n| format!("\"etag-{n}\"")).collect::<Vec<_>>() })])
			}),
		)
		.route(
			"/v1/volume-backups/restore",
			post(|State(a): State<Arc<Agent>>, body: Bytes| async move {
				let req: Value = serde_json::from_slice(&body).unwrap();
				a.log(format!(
					"restore {} wipe={}",
					req["volume"].as_str().unwrap(),
					req["wipe"]
				));
				// The checksum the control plane sends is the one it recorded.
				a.log(format!("sha256 {}", req["sha256"].as_str().unwrap()));
				if a.fail_restore.load(Ordering::SeqCst) {
					return ndjson(&[
						json!({ "type": "failed", "message": "extracting: disk full" }),
					]);
				}
				ndjson(&[json!({ "type": "done" })])
			}),
		)
		.with_state(agent.clone());
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	agent
}

/// A fake object store: the three multipart requests and deletes, recorded.
pub struct S3 {
	pub calls: Mutex<Vec<String>>,
	pub port: u16,
}

impl S3 {
	pub fn calls(&self) -> Vec<String> {
		self.calls.lock().unwrap().clone()
	}

	pub fn deleted(&self) -> Vec<String> {
		self.calls()
			.into_iter()
			.filter_map(|c| c.strip_prefix("delete ").map(str::to_owned))
			.collect()
	}
}

pub async fn fake_s3() -> Arc<S3> {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let s3 = Arc::new(S3 {
		calls: Mutex::new(Vec::new()),
		port: listener.local_addr().unwrap().port(),
	});
	let recorder = s3.clone();
	let app = Router::new().fallback(move |req: Request| {
		let s3 = recorder.clone();
		async move {
			let method = req.method().as_str().to_owned();
			let path = req.uri().path().to_owned();
			let query = req.uri().query().unwrap_or("").to_owned();
			let key = path
				.trim_start_matches('/')
				.trim_start_matches(&format!("{BUCKET}/"))
				.to_owned();
			let xml =
				|body: String| ([(header::CONTENT_TYPE, "application/xml")], body).into_response();
			let ns = "http://s3.amazonaws.com/doc/2006-03-01/";
			match (method.as_str(), query.as_str()) {
				("POST", q) if q.contains("uploads") => {
					s3.calls.lock().unwrap().push(format!("create {key}"));
					xml(format!(
						"<InitiateMultipartUploadResult xmlns=\"{ns}\"><Bucket>{BUCKET}</Bucket><Key>{key}</Key><UploadId>up-1</UploadId></InitiateMultipartUploadResult>"
					))
				}
				("POST", q) if q.contains("uploadId") => {
					let body = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap();
					let parts = String::from_utf8_lossy(&body).matches("<Part>").count();
					s3.calls.lock().unwrap().push(format!("complete {key} parts={parts}"));
					xml(format!(
						"<CompleteMultipartUploadResult xmlns=\"{ns}\"><Location>http://x/{key}</Location><Bucket>{BUCKET}</Bucket><Key>{key}</Key><ETag>\"all\"</ETag></CompleteMultipartUploadResult>"
					))
				}
				("DELETE", q) if q.contains("uploadId") => {
					s3.calls.lock().unwrap().push(format!("abort {key}"));
					StatusCode::NO_CONTENT.into_response()
				}
				("DELETE", _) => {
					s3.calls.lock().unwrap().push(format!("delete {key}"));
					StatusCode::NO_CONTENT.into_response()
				}
				_ => StatusCode::BAD_REQUEST.into_response(),
			}
		}
	});
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	s3
}

// --- the fixture ---------------------------------------------------------------------------

pub struct Fx {
	pub state: AppState,
	pub app: Router,
	pub db: PgPool,
	pub agent: Arc<Agent>,
	pub s3: Arc<S3>,
	pub token: String,
	pub team: i64,
	pub project: i64,
	pub application: i64,
	pub volume: i64,
	pub volume_name: String,
	pub target: i64,
}

pub const CONTAINER: &str = "ctr-web-1";

impl Fx {
	/// Alice, a project, an image application `web` running in container `ctr-web-1`
	/// with a volume at `/data`, and a destination on the fake object store.
	pub async fn new(db: PgPool) -> Self {
		Self::build(db, true).await
	}

	/// `fake` serves the recorded agent and a running deployment; without it the socket
	/// is left for the real agent and the application is not running.
	pub async fn build(db: PgPool, fake: bool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		Self::over(db, state, app, fake).await
	}

	/// [`Fx::build`] over a router somebody else built on `state`: `podkit-ui` passes the
	/// one with the pages.
	pub async fn over(db: PgPool, state: AppState, app: Router, fake: bool) -> Self {
		let agent = if fake {
			fake_agent(&state)
		} else {
			Arc::new(Agent::default())
		};
		let s3 = fake_s3().await;
		let (token, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "shop", "slug": "shop" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s} {project}");
		let project = project["id"].as_i64().unwrap();
		let server = server_id(&db, team).await;
		let (s, created) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(&token),
			Some(
				json!({ "name": "web", "slug": "web", "server_id": server, "container_port": 80,
				"image": "docker.io/traefik/whoami" }),
			),
		)
		.await;
		assert!(s.is_success(), "create application: {s} {created}");
		let application = created["id"].as_i64().unwrap();
		let (s, volume) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications/{application}/volumes"),
			Some(&token),
			Some(json!({ "mount_path": "/data" })),
		)
		.await;
		assert!(s.is_success(), "create volume: {s} {volume}");
		if fake {
			insert_deployment(
				&db,
				application,
				9001,
				"running",
				Some("img"),
				Some(CONTAINER),
			)
			.await;
		}

		let secret = state.secrets.encrypt(SECRET).unwrap();
		let target: i64 = sqlx::query_scalar(
			"INSERT INTO backup_targets (id, team_id, name, endpoint, region, bucket, path_style,
				prefix, access_key_id, secret_access_key)
			VALUES ($1, $2, 'garage', $3, 'us-east-1', $4, TRUE, 'pre', 'AKIDEXAMPLE', $5)
			RETURNING id",
		)
		.bind(7001_i64)
		.bind(team)
		.bind(format!("http://127.0.0.1:{}", s3.port))
		.bind(BUCKET)
		.bind(secret)
		.fetch_one(&db)
		.await
		.unwrap();
		Self {
			state,
			app,
			db,
			agent,
			s3,
			token,
			team,
			project,
			application,
			volume: volume["id"].as_i64().unwrap(),
			volume_name: volume["name"].as_str().unwrap().to_owned(),
			target,
		}
	}

	pub fn api(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/volumes/{}/backups{rest}",
			self.team, self.project, self.application, self.volume
		)
	}

	pub fn ui(&self, rest: &str) -> String {
		format!(
			"/teams/{}/projects/{}/apps/{}/volumes{rest}",
			self.team, self.project, self.application
		)
	}

	pub async fn call(&self, method: &str, rest: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, &self.api(rest), Some(&self.token), body).await
	}

	/// Points the volume at the destination with `extra` merged over the settings.
	pub async fn configure(&self, extra: Value) -> (StatusCode, Value) {
		let mut body = json!({ "target_id": self.target, "keep": 7, "consistency": "live" });
		for (k, v) in extra.as_object().unwrap() {
			body[k] = v.clone();
		}
		self.call("PUT", "/settings", Some(body)).await
	}

	/// Starts a backup through the API and runs its job the way the worker does.
	pub async fn back_up(&self) -> (i64, Result<(), JobFailure>) {
		let (s, backup) = self.call("POST", "", None).await;
		assert_eq!(s, StatusCode::ACCEPTED, "{backup}");
		let id = backup["id"].as_i64().unwrap();
		let job = jobs::latest_for(&self.db, JobKind::VolumeBackup, id)
			.await
			.unwrap()
			.expect("a backup job is queued");
		(id, self.run(job).await)
	}

	/// Runs `job` and, on failure, reflects it on its row like the worker.
	pub async fn run(&self, job: Job) -> Result<(), JobFailure> {
		sqlx::query("UPDATE jobs SET status = 'running' WHERE id = $1")
			.bind(job.id)
			.execute(&self.db)
			.await
			.unwrap();
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let cancel = CancellationToken::new();
		let result = match job.kind {
			JobKind::VolumeBackup => {
				crate::volume_backups::run::run_backup(&self.state, &job, &cancel, &mut log).await
			}
			JobKind::VolumeRestore => {
				crate::volume_backups::run::run_restore(&self.state, &job, &cancel, &mut log).await
			}
			other => panic!("not a volume job: {other:?}"),
		};
		log.finish().await;
		if let Err(failure) = &result {
			jobs::fail_target(&self.state, job.kind, job.target_id, failure).await;
		}
		sqlx::query("UPDATE jobs SET status = $2 WHERE id = $1")
			.bind(job.id)
			.bind(if result.is_ok() {
				"succeeded"
			} else {
				"failed"
			})
			.execute(&self.db)
			.await
			.unwrap();
		result
	}

	pub async fn backup_row(
		&self,
		id: i64,
	) -> (String, Option<String>, Option<String>, Option<i64>) {
		sqlx::query_as("SELECT status, error, warning, entries FROM volume_backups WHERE id = $1")
			.bind(id)
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	pub async fn backup_count(&self) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM volume_backups")
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	pub async fn restore(&self, backup: i64, body: Value) -> (StatusCode, Value) {
		self.call("POST", &format!("/{backup}/restore"), Some(body))
			.await
	}

	pub async fn restore_job(&self, restore: i64) -> Job {
		jobs::latest_for(&self.db, JobKind::VolumeRestore, restore)
			.await
			.unwrap()
			.expect("a restore job is queued")
	}

	pub async fn restore_status(&self, restore: i64) -> (String, Option<String>) {
		sqlx::query_as("SELECT status, error FROM volume_restores WHERE id = $1")
			.bind(restore)
			.fetch_one(&self.db)
			.await
			.unwrap()
	}
}

// --- restore -------------------------------------------------------------------------------

/// A succeeded backup, made through the job.
pub async fn backed_up(f: &Fx) -> i64 {
	f.configure(json!({})).await;
	let (id, result) = f.back_up().await;
	result.unwrap();
	f.agent.clear();
	id
}
