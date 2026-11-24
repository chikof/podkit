//! API and job tests against a stub of the Hugging Face hub and a fake agent
//! on the local server's socket. Nothing here touches the network or Podman.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, post};
use proto::{Disk, GATED_MODEL_MESSAGE, Hardware};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;
use tokio_util::sync::CancellationToken;

use super::hf::tests::{GOOD_TOKEN, stub};
use crate::jobs::{self, JobKind, JobStatus};
use crate::state::AppState;
use crate::tests::{call, state, user_with_team};

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	token: String,
	team: i64,
	server: i64,
}

impl Setup {
	async fn new(db: PgPool, free_mb: u64) -> Self {
		Self::with_hf(db, free_mb, stub().await).await
	}

	async fn with_hf(db: PgPool, free_mb: u64, hf_endpoint: String) -> Self {
		let mut state = state(db.clone());
		std::sync::Arc::make_mut(&mut state.config).hf_endpoint = hf_endpoint;
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let server = servers[0]["id"].as_i64().unwrap();
		let setup = Self {
			app,
			state,
			db,
			token,
			team,
			server,
		};
		setup.set_free_mb(free_mb).await;
		setup
	}

	async fn set_free_mb(&self, free_mb: u64) {
		let hardware = Hardware {
			disks: vec![Disk {
				path: "/data".to_owned(),
				free_mb,
				total_mb: free_mb * 2,
			}],
			..Hardware::default()
		};
		sqlx::query("UPDATE servers SET hardware = $2 WHERE id = $1")
			.bind(self.server)
			.bind(Json(hardware))
			.execute(&self.db)
			.await
			.unwrap();
	}

	fn path(&self, rest: &str) -> String {
		format!("/api/v1/teams/{}/{rest}", self.team)
	}

	async fn call(&self, method: &str, rest: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, &self.path(rest), Some(&self.token), body).await
	}

	async fn create(&self, repo: &str, include: &[&str]) -> (StatusCode, Value) {
		self.call(
			"POST",
			"model-files",
			Some(json!({ "server_id": self.server, "repo": repo, "include": include })),
		)
		.await
	}

	async fn count(&self, table: &str) -> i64 {
		sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	async fn set_status(&self, id: i64, status: &str, downloaded: i64) {
		sqlx::query("UPDATE model_files SET status = $2, downloaded_bytes = $3 WHERE id = $1")
			.bind(id)
			.bind(status)
			.bind(downloaded)
			.execute(&self.db)
			.await
			.unwrap();
	}
}

const GB: u64 = 1_000_000_000;

#[sqlx::test]
async fn create_checks_size_and_queues_a_download(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let (status, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	assert_eq!(status, StatusCode::OK, "{file}");

	let id = file["id"].as_i64().unwrap();
	assert_eq!(file["status"], "queued");
	assert_eq!(file["repo"], "acme/open");
	assert_eq!(file["revision"], "main");
	assert_eq!(file["total_bytes"], 2 * GB);
	assert_eq!(file["downloaded_bytes"], 0);
	assert_eq!(file["volume_name"], format!("model-files-{id}"));
	assert_eq!(
		file["selection"],
		json!({ "include": ["*Q4_K_M.gguf"], "files": [] })
	);
	assert_eq!(file["server_id"], s.server);

	let job = jobs::latest_for(&s.db, JobKind::ModelDownload, id)
		.await
		.unwrap()
		.expect("a download job is queued");
	assert_eq!(job.status, JobStatus::Queued);
	assert_eq!(job.lock_key, format!("model-file:{id}"));
	assert_eq!(job.server_id.0, s.server);

	let (status, shown) = s.call("GET", &format!("model-files/{id}"), None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(shown, file);
	let (_, listed) = s.call("GET", "model-files", None).await;
	assert_eq!(listed.as_array().unwrap().len(), 1);

	// The same files again are refused instead of downloading twice.
	let (status, body) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn a_download_that_does_not_fit_is_refused_before_any_job(db: PgPool) {
	// 2 GB needs 2.2 GB with headroom, which is 2098 MiB.
	let s = Setup::new(db, 2_000).await;
	let (status, body) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("not enough disk space"),
		"{body}"
	);
	assert_eq!(s.count("model_files").await, 0);
	assert_eq!(s.count("jobs").await, 0);

	s.set_free_mb(2_100).await;
	let (status, body) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(s.count("jobs").await, 1);

	// The whole repo is 6 GB: it does not fit in what is left.
	s.set_free_mb(5_000).await;
	let (status, _) = s.create("acme/open", &[]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(s.count("model_files").await, 1);
}

#[sqlx::test]
async fn a_server_without_a_disk_report_is_not_guessed_at(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	sqlx::query("UPDATE servers SET hardware = NULL")
		.execute(&s.db)
		.await
		.unwrap();
	let (status, _) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	assert_eq!(status, StatusCode::CONFLICT);
	assert_eq!(s.count("jobs").await, 0);
}

#[sqlx::test]
async fn gated_missing_and_ok_outcomes(db: PgPool) {
	let s = Setup::new(db, 100_000).await;

	let (status, body) = s.create("acme/gated", &[]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(body["error"], GATED_MODEL_MESSAGE);

	let (status, body) = s.create("acme/does-not-exist", &[]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert!(
		body["error"].as_str().unwrap().contains("not found"),
		"{body}"
	);

	let (status, body) = s.create("acme/open", &["*.safetensors"]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert!(
		body["error"].as_str().unwrap().contains("no files"),
		"{body}"
	);

	let (status, body) = s
		.call(
			"POST",
			"model-files",
			Some(json!({ "server_id": s.server, "repo": "acme/open", "files": ["nope.gguf"] })),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert!(
		body["error"].as_str().unwrap().contains("nope.gguf"),
		"{body}"
	);

	// A wrong token is the same as none; the right one opens the repo.
	s.call(
		"PUT",
		"huggingface-token",
		Some(json!({ "token": "hf_wrong" })),
	)
	.await;
	let (status, body) = s.create("acme/gated", &[]).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(body["error"], GATED_MODEL_MESSAGE);

	s.call(
		"PUT",
		"huggingface-token",
		Some(json!({ "token": GOOD_TOKEN })),
	)
	.await;
	let (status, body) = s.create("acme/gated", &[]).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["total_bytes"], 3_000);
	assert_eq!(s.count("model_files").await, 1);
}

#[sqlx::test]
async fn input_is_validated(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	for (repo, include) in [
		("", vec![]),
		("--help", vec![]),
		("a/b/c", vec![]),
		("acme/open", vec!["--token=x"]),
		("acme/open", vec!["[a].gguf"]),
	] {
		let (status, body) = s.create(repo, &include).await;
		assert_eq!(
			status,
			StatusCode::BAD_REQUEST,
			"{repo} {include:?}: {body}"
		);
	}
	let (status, _) = s
		.call(
			"POST",
			"model-files",
			Some(json!({ "server_id": s.server, "repo": "acme/open", "revision": "../x" })),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(s.count("jobs").await, 0);
}

#[sqlx::test]
async fn the_token_is_write_only(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let secret = "hf_supersecretvalue123";

	let (status, body) = s.call("GET", "huggingface-token", None).await;
	assert_eq!(
		(status, body),
		(StatusCode::OK, json!({ "configured": false }))
	);

	let (status, put) = s
		.call("PUT", "huggingface-token", Some(json!({ "token": secret })))
		.await;
	assert_eq!(
		(status, &put),
		(StatusCode::OK, &json!({ "configured": true }))
	);
	let (_, got) = s.call("GET", "huggingface-token", None).await;
	assert_eq!(got, json!({ "configured": true }));

	// Stored encrypted.
	let blob: Vec<u8> = sqlx::query_scalar("SELECT token FROM huggingface_tokens")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&blob).contains(secret));
	assert_eq!(s.state.secrets.decrypt(&blob).unwrap(), secret);

	// Nothing a model-file endpoint returns carries it.
	let (_, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	let id = file["id"].as_i64().unwrap();
	let (_, shown) = s.call("GET", &format!("model-files/{id}"), None).await;
	let (_, listed) = s.call("GET", "model-files", None).await;
	for body in [file, shown, listed, put, got] {
		assert!(!body.to_string().contains(secret), "{body}");
	}

	for bad in ["", "   ", "a b", "a\nb"] {
		let (status, _) = s
			.call("PUT", "huggingface-token", Some(json!({ "token": bad })))
			.await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
	}

	let (status, body) = s.call("DELETE", "huggingface-token", None).await;
	assert_eq!(
		(status, body),
		(StatusCode::OK, json!({ "configured": false }))
	);
	let (_, got) = s.call("GET", "huggingface-token", None).await;
	assert_eq!(got, json!({ "configured": false }));
	assert_eq!(s.count("huggingface_tokens").await, 0);
}

#[sqlx::test]
async fn other_teams_get_not_found(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let (_, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	let id = file["id"].as_i64().unwrap();
	let (bob, bob_team) = user_with_team(&s.app, "bob").await;
	s.set_status(id, "failed", 0).await;

	let bob_path = |rest: &str| format!("/api/v1/teams/{bob_team}/{rest}");
	for (method, rest) in [
		("GET", format!("model-files/{id}")),
		("DELETE", format!("model-files/{id}")),
		("POST", format!("model-files/{id}/retry")),
		("POST", format!("model-files/{id}/cancel")),
	] {
		let (status, _) = call(
			&s.app,
			method,
			&bob_path(&rest),
			Some(&bob),
			Some(json!({})),
		)
		.await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {rest}");
	}
	// The same ids through Alice's team path are 404 for Bob: the team is not there for him.
	let (status, _) = call(
		&s.app,
		"GET",
		&s.path(&format!("model-files/{id}")),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// Bob cannot target Alice's server, and sees none of her files.
	let (status, _) = call(
		&s.app,
		"POST",
		&bob_path("model-files"),
		Some(&bob),
		Some(json!({ "server_id": s.server, "repo": "acme/open" })),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (_, listed) = call(&s.app, "GET", &bob_path("model-files"), Some(&bob), None).await;
	assert_eq!(listed, json!([]));
	assert_eq!(s.count("model_files").await, 1);
}

#[sqlx::test]
async fn members_can_read_but_not_change(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let (_, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	let id = file["id"].as_i64().unwrap();

	let (carol, _) = user_with_team(&s.app, "carol").await;
	let carol_id: i64 =
		sqlx::query_scalar("SELECT id FROM users WHERE email = 'carol@example.com'")
			.fetch_one(&s.db)
			.await
			.unwrap();
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(s.team)
		.bind(carol_id)
		.execute(&s.db)
		.await
		.unwrap();

	let as_carol = |method: &'static str, rest: String, body: Option<Value>| {
		let (app, path, carol) = (s.app.clone(), s.path(&rest), carol.clone());
		async move { call(&app, method, &path, Some(&carol), body).await.0 }
	};
	assert_eq!(
		as_carol("GET", "model-files".into(), None).await,
		StatusCode::OK
	);
	assert_eq!(
		as_carol("GET", format!("model-files/{id}"), None).await,
		StatusCode::OK
	);
	assert_eq!(
		as_carol("GET", "huggingface-token".into(), None).await,
		StatusCode::OK
	);
	let create = Some(json!({ "server_id": s.server, "repo": "acme/open" }));
	assert_eq!(
		as_carol("POST", "model-files".into(), create).await,
		StatusCode::FORBIDDEN
	);
	for (method, rest, body) in [
		("DELETE", format!("model-files/{id}"), None),
		("POST", format!("model-files/{id}/retry"), None),
		("POST", format!("model-files/{id}/cancel"), None),
		(
			"PUT",
			"huggingface-token".to_owned(),
			Some(json!({ "token": "hf_x" })),
		),
		("DELETE", "huggingface-token".to_owned(), None),
	] {
		assert_eq!(
			as_carol(method, rest.clone(), body).await,
			StatusCode::FORBIDDEN,
			"{method} {rest}"
		);
	}
}

#[sqlx::test]
async fn retry_resumes_and_only_counts_what_is_missing(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let (_, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	let id = file["id"].as_i64().unwrap();
	let retry = format!("model-files/{id}/retry");

	// Only failed downloads retry.
	let (status, _) = s.call("POST", &retry, None).await;
	assert_eq!(status, StatusCode::CONFLICT);

	// 1.9 of 2 GB are on disk, so 0.1 GB (plus headroom) must fit, not 2 GB.
	s.set_status(id, "failed", 1_900_000_000).await;
	s.set_free_mb(200).await;
	let (status, body) = s.call("POST", &retry, None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["status"], "queued");
	assert_eq!(body["downloaded_bytes"], 1_900_000_000);
	assert_eq!(body["error"], Value::Null);
	assert_eq!(s.count("jobs").await, 2);

	// With nothing downloaded the same free space is not enough.
	s.set_status(id, "failed", 0).await;
	let (status, _) = s.call("POST", &retry, None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(s.count("jobs").await, 2);
}

#[sqlx::test]
async fn cancel_stops_a_queued_download(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let (_, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	let id = file["id"].as_i64().unwrap();

	let (status, body) = s
		.call("POST", &format!("model-files/{id}/cancel"), None)
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["status"], "failed");
	assert_eq!(body["error"], "cancelled");
	let job = jobs::latest_for(&s.db, JobKind::ModelDownload, id)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(job.status, JobStatus::Cancelled);

	let (status, _) = s
		.call("POST", &format!("model-files/{id}/cancel"), None)
		.await;
	assert_eq!(status, StatusCode::CONFLICT);
}

/// What the fake agent saw and what it answers.
#[derive(Default)]
struct FakeAgent {
	downloads: Mutex<Vec<Value>>,
	deleted_volumes: Mutex<Vec<String>>,
}

/// Serves the two agent routes this slice uses on the local server's socket.
/// `download_events` is the NDJSON the download stream returns.
fn fake_agent(state: &AppState, download_events: &[Value]) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	let (for_download, for_delete) = (agent.clone(), agent.clone());
	let ndjson = download_events.iter().fold(String::new(), |mut out, e| {
		out.push_str(&e.to_string());
		out.push('\n');
		out
	});
	let app = Router::new()
		.route(
			"/v1/model-downloads",
			post(move |body: Bytes| async move {
				for_download
					.downloads
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				([(header::CONTENT_TYPE, "application/x-ndjson")], ndjson).into_response()
			}),
		)
		.route(
			"/v1/volumes/{name}",
			delete(move |Path(name): Path<String>| async move {
				for_delete.deleted_volumes.lock().unwrap().push(name);
				StatusCode::NO_CONTENT
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	agent
}

async fn run_job(s: &Setup, id: i64) -> (Result<(), jobs::JobFailure>, String) {
	let job = jobs::latest_for(&s.db, JobKind::ModelDownload, id)
		.await
		.unwrap()
		.unwrap();
	let mut log = s.state.logs.open(job.id).await.unwrap();
	let result = super::run::run(&s.state, &job, &CancellationToken::new(), &mut log).await;
	log.finish().await;
	let text = std::fs::read_to_string(s.state.logs.path(job.id)).unwrap();
	(result, text)
}

#[sqlx::test]
async fn the_job_downloads_reports_progress_and_never_logs_the_token(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	s.call(
		"PUT",
		"huggingface-token",
		Some(json!({ "token": GOOD_TOKEN })),
	)
	.await;
	let (_, file) = s.create("acme/gated", &["*.gguf"]).await;
	let id = file["id"].as_i64().unwrap();
	let agent = fake_agent(
		&s.state,
		&[
			// A tool that echoes its environment must not leak it into the job log.
			json!({ "type": "log", "stream": "stderr", "line": format!("auth: {GOOD_TOKEN}") }),
			json!({ "type": "progress", "bytes": 1_500 }),
			json!({ "type": "progress", "bytes": 99_999 }),
			json!({ "type": "done" }),
		],
	);

	let (result, log) = run_job(&s, id).await;
	result.unwrap();

	let sent = agent.downloads.lock().unwrap().clone();
	assert_eq!(sent.len(), 1);
	assert_eq!(sent[0]["volume"], format!("model-files-{id}"));
	assert_eq!(sent[0]["repo"], "acme/gated");
	assert_eq!(sent[0]["revision"], "main");
	assert_eq!(sent[0]["include"], json!(["*.gguf"]));
	assert_eq!(
		sent[0]["token"], GOOD_TOKEN,
		"the agent gets the token to pass on"
	);

	assert!(!log.contains(GOOD_TOKEN), "token in job log: {log}");
	assert!(log.contains("auth: ***"), "{log}");
	assert!(log.contains("(50%)"), "progress is logged: {log}");

	let (_, done) = s.call("GET", &format!("model-files/{id}"), None).await;
	assert_eq!(done["status"], "ready");
	assert_eq!(done["downloaded_bytes"], done["total_bytes"]);
	assert_eq!(done["error"], Value::Null);
}

#[sqlx::test]
async fn a_failed_job_marks_the_row_failed_without_the_token(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	s.call(
		"PUT",
		"huggingface-token",
		Some(json!({ "token": GOOD_TOKEN })),
	)
	.await;
	let (_, file) = s.create("acme/gated", &[]).await;
	let id = file["id"].as_i64().unwrap();
	fake_agent(
		&s.state,
		&[
			json!({ "type": "progress", "bytes": 1_000 }),
			json!({ "type": "failed", "message": format!("boom with {GOOD_TOKEN}") }),
		],
	);

	let (result, _) = run_job(&s, id).await;
	let jobs::JobFailure::Failed(message) = result.unwrap_err() else {
		panic!("expected a failure");
	};
	assert_eq!(message, "boom with ***");
	// The worker records it on the row.
	jobs::fail_target(
		&s.state,
		JobKind::ModelDownload,
		id,
		&jobs::JobFailure::Failed(message),
	)
	.await;
	let (_, shown) = s.call("GET", &format!("model-files/{id}"), None).await;
	assert_eq!(shown["status"], "failed");
	assert_eq!(shown["error"], "boom with ***");
	// Progress made before the failure stays, which is what a retry resumes from.
	assert_eq!(shown["downloaded_bytes"], 1_000);
}

#[sqlx::test]
async fn delete_removes_the_volume_then_the_row(db: PgPool) {
	let s = Setup::new(db, 100_000).await;
	let (_, file) = s.create("acme/open", &["*Q4_K_M.gguf"]).await;
	let id = file["id"].as_i64().unwrap();
	let path = format!("model-files/{id}");

	// Still queued: cancel it first.
	let (status, _) = s.call("DELETE", &path, None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	s.set_status(id, "ready", 2_000_000_000).await;

	// The agent is down: the row stays so the volume is not orphaned.
	let (status, _) = s.call("DELETE", &path, None).await;
	assert!(status.is_server_error(), "{status}");
	assert_eq!(s.count("model_files").await, 1);

	let agent = fake_agent(&s.state, &[]);
	let (status, body) = s.call("DELETE", &path, None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["id"], id);
	assert_eq!(
		*agent.deleted_volumes.lock().unwrap(),
		vec![format!("model-files-{id}")]
	);
	assert_eq!(s.count("model_files").await, 0);
	let (status, _) = s.call("GET", &path, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The whole path against the real Hugging Face and a real podman: the agent
/// runs in this process on the local server's socket.
///
/// `cargo test -p server --all-features -- --ignored live_download`
#[sqlx::test]
#[ignore = "downloads about 1 MB from huggingface.co with a real podman.sock"]
async fn live_download_of_a_small_gguf(db: PgPool) {
	let s = Setup::with_hf(db, 100_000, "https://huggingface.co".to_owned()).await;

	let podman = agent::podman::Podman::connect(&agent::podman::local_socket_path())
		.expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&s.state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let (status, file) = s
		.call(
			"POST",
			"model-files",
			Some(json!({
				"server_id": s.server,
				"repo": "ggml-org/tiny-llamas",
				"files": ["stories260K.gguf"],
			})),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{file}");
	let id = file["id"].as_i64().unwrap();
	assert_eq!(file["total_bytes"], 1_185_376);

	let (result, log) = run_job(&s, id).await;
	result.unwrap_or_else(|e| panic!("download failed: {e:?}\n{log}"));

	// The proof from the card: status ready and every byte there.
	let (_, done) = s.call("GET", &format!("model-files/{id}"), None).await;
	assert_eq!(done["status"], "ready", "{done}");
	assert_eq!(done["downloaded_bytes"], done["total_bytes"], "{done}");

	let (status, _) = s.call("DELETE", &format!("model-files/{id}"), None).await;
	assert_eq!(status, StatusCode::OK);
	let volume = format!("model-files-{id}");
	assert!(
		podman.docker().inspect_volume(&volume).await.is_err(),
		"the volume is removed with the model file"
	);
}
