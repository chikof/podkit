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
