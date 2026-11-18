mod tenancy;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use crypto::age::{SecretBox, generate_identity};
use serde_json::Value;
pub use sqlx::PgPool;
pub use sqlx::test as db_test;
use tower::ServiceExt;

use crate::agents::AgentPool;
use crate::jobs::worker::Jobs;
use crate::logs::LogHub;
use crate::state::{AppState, Config};

pub async fn app(db: PgPool) -> Router {
	crate::routes::routes(state(db)).await.unwrap()
}

/// A full `AppState` over `db` with a throwaway data dir and no live agent.
pub fn state(db: PgPool) -> AppState {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let data_dir = tempfile::tempdir().unwrap().keep();
	let log_dir = data_dir.join("logs").join("jobs");
	std::fs::create_dir_all(&log_dir).unwrap();
	let local_agent_socket = data_dir.join("agent.sock");
	AppState {
		db,
		secrets: Arc::new(SecretBox::from_identity_str(&generate_identity()).unwrap()),
		agents: Arc::new(AgentPool::new(local_agent_socket.clone())),
		jobs: Arc::new(Jobs::new()),
		logs: Arc::new(LogHub::new(log_dir)),
		http: reqwest::Client::new(),
		shutdown: tokio_util::sync::CancellationToken::new(),
		config: Arc::new(Config {
			ingress_port: 8088,
			https_port: 8443,
			acme_email: None,
			agent_binaries_dir: data_dir.clone(),
			hf_endpoint: "http://127.0.0.1:1".to_owned(),
			model_ready_timeout: None,
			model_ready_interval: std::time::Duration::from_millis(20),
			local_agent_socket,
			data_dir,
		}),
	}
}

pub async fn call(
	app: &Router,
	method: &str,
	uri: &str,
	token: Option<&str>,
	body: Option<Value>,
) -> (StatusCode, Value) {
	let mut req = Request::builder().method(method).uri(uri);
	if let Some(t) = token {
		req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
	}
	let req = match body {
		Some(b) => req
			.header(header::CONTENT_TYPE, "application/json")
			.body(Body::from(b.to_string())),
		None => req.body(Body::empty()),
	}
	.unwrap();
	let resp = app.clone().oneshot(req).await.unwrap();
	let status = resp.status();
	let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
	(status, json)
}

/// Registers a user, logs in, creates a team; returns (token, team id).
pub async fn user_with_team(app: &Router, name: &str) -> (String, i64) {
	let email = format!("{name}@example.com");
	let creds =
		serde_json::json!({ "name": name, "email": email, "password": "correct horse battery" });
	let (s, _) = call(
		app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(creds.clone()),
	)
	.await;
	assert!(s.is_success(), "register {name}: {s}");
	let (s, login) = call(app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
	assert_eq!(s, StatusCode::OK, "login {name}");
	let token = login["token"].as_str().unwrap().to_owned();
	let (s, team) = call(
		app,
		"POST",
		"/api/v1/teams",
		Some(token.as_str()),
		Some(serde_json::json!({ "name": name, "slug": name })),
	)
	.await;
	assert!(s.is_success(), "create team {name}: {s}");
	(token, team["id"].as_i64().unwrap())
}

/// Rows in `table`. For asserting that a refused request changed nothing.
pub async fn count(db: &PgPool, table: &str) -> i64 {
	sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
		.fetch_one(db)
		.await
		.unwrap()
}

/// The id of the project with `slug`.
pub async fn project_id(db: &PgPool, slug: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM projects WHERE slug = $1")
		.bind(slug)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Puts an application and a database straight into `project`, on its team's
/// local server, so pages that list them have something to show.
pub async fn seed_app_and_database(db: &PgPool, team: i64, project: i64) {
	let server: i64 = sqlx::query_scalar("SELECT id FROM servers WHERE team_id = $1")
		.bind(team)
		.fetch_one(db)
		.await
		.unwrap();
	sqlx::query(
		"INSERT INTO applications (id, project_id, server_id, name, slug, repo_url, container_port)
		 VALUES (5001, $1, $2, 'Storefront', 'storefront', 'https://example.com/r.git', 3000)",
	)
	.bind(project)
	.bind(server)
	.execute(db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials)
		 VALUES (6001, $1, $2, 'orders', 'postgres', '17', 'x')",
	)
	.bind(project)
	.bind(server)
	.execute(db)
	.await
	.unwrap();
}

/// Gives every server of `team` the hardware snapshot `hardware`.
pub async fn set_hardware(db: &PgPool, team: i64, hardware: &proto::Hardware) {
	sqlx::query("UPDATE servers SET hardware = $2 WHERE team_id = $1")
		.bind(team)
		.bind(sqlx::types::Json(hardware))
		.execute(db)
		.await
		.unwrap();
}

/// The id of the team's (local) server.
pub async fn server_id(db: &PgPool, team: i64) -> i64 {
	sqlx::query_scalar("SELECT id FROM servers WHERE team_id = $1")
		.bind(team)
		.fetch_one(db)
		.await
		.unwrap()
}

/// A ready download and a model serving it, in `project` on the team's
/// server. Returns (model file id, model id).
pub async fn seed_model(
	db: &PgPool,
	team: i64,
	project: i64,
	name: &str,
	status: &str,
	container_id: Option<&str>,
) -> (i64, i64) {
	let server = server_id(db, team).await;
	let (files, model) = (crypto::generate_id(), crypto::generate_id());
	sqlx::query(
		"INSERT INTO model_files
			(id, team_id, server_id, repo, revision, selection, total_bytes, downloaded_bytes,
			 status, volume_name)
		VALUES ($1, $2, $3, 'acme/open', 'main', '{\"include\":[],\"files\":[\"q.gguf\"]}', 2000000000,
			2000000000, 'ready', $4)",
	)
	.bind(files)
	.bind(team)
	.bind(server)
	.bind(format!("model-files-{files}"))
	.execute(db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO models
			(id, project_id, server_id, model_files_id, name, slug, engine, image, port,
			 model_path, served_name, context_size, status, container_id)
		VALUES ($1, $2, $3, $4, $5, $6, 'llama_cpu', 'img', 8080, 'q.gguf', $7, 4096, $8, $9)",
	)
	.bind(model)
	.bind(project)
	.bind(server)
	.bind(files)
	.bind(name)
	.bind(format!("slug-{model}"))
	.bind(format!("served-{model}"))
	.bind(status)
	.bind(container_id)
	.execute(db)
	.await
	.unwrap();
	(files, model)
}

/// Sets a download's status and progress.
pub async fn set_file_status(db: &PgPool, id: i64, status: &str, downloaded: i64) {
	sqlx::query("UPDATE model_files SET status = $2, downloaded_bytes = $3 WHERE id = $1")
		.bind(id)
		.bind(status)
		.bind(downloaded)
		.execute(db)
		.await
		.unwrap();
}

/// Forgets what every server reported about its hardware.
pub async fn clear_hardware(db: &PgPool) {
	sqlx::query("UPDATE servers SET hardware = NULL")
		.execute(db)
		.await
		.unwrap();
}

/// Sets a model's status and error.
pub async fn set_model_status(db: &PgPool, id: i64, status: &str, error: Option<&str>) {
	sqlx::query("UPDATE models SET status = $2, error = $3 WHERE id = $1")
		.bind(id)
		.bind(status)
		.bind(error)
		.execute(db)
		.await
		.unwrap();
}

/// Marks a download failed with `error`.
pub async fn fail_file(db: &PgPool, id: i64, error: &str) {
	sqlx::query("UPDATE model_files SET status = 'failed', error = $2 WHERE id = $1")
		.bind(id)
		.bind(error)
		.execute(db)
		.await
		.unwrap();
}

/// The id of the team with `slug`.
pub async fn team_id(db: &PgPool, slug: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM teams WHERE slug = $1")
		.bind(slug)
		.fetch_one(db)
		.await
		.unwrap()
}

/// One hour of gateway usage, `hours_ago` hours back.
#[allow(clippy::too_many_arguments, reason = "a row of model_usage")]
pub async fn seed_usage(
	db: &PgPool,
	key_hash: &str,
	model: i64,
	hours_ago: i32,
	requests: i64,
	input: i64,
	output: i64,
	application: Option<i64>,
) {
	sqlx::query(
		"INSERT INTO model_usage
			(key_hash, model_id, hour, application_id, requests, input_tokens, output_tokens)
		VALUES ($1, $2, date_trunc('hour', NOW()) - make_interval(hours => $3), $4, $5, $6, $7)",
	)
	.bind(key_hash)
	.bind(model)
	.bind(hours_ago)
	.bind(application)
	.bind(requests)
	.bind(input)
	.bind(output)
	.execute(db)
	.await
	.unwrap();
}

/// The hash of a model's playground key.
pub async fn playground_hash(db: &PgPool, model: i64) -> String {
	sqlx::query_scalar("SELECT key_hash FROM playground_keys WHERE model_id = $1")
		.bind(model)
		.fetch_one(db)
		.await
		.unwrap()
}
