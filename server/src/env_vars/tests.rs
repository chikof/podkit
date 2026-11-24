//! The env var API: the bulk `.env` endpoint the command line pushes with.
//! `cargo test -p server --all-features -- env_vars_api`

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::tests::{app, call, local_server_id, user_with_team};

/// A project with one image application in `team`; returns the env-vars URL.
async fn env_url(app: &Router, db: &PgPool, session: &str, team: i64) -> String {
	let (s, p) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(session),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert!(s.is_success(), "create project: {s} {p}");
	let p = p["id"].as_i64().unwrap();
	let server = local_server_id(db, team).await;
	let (s, a) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects/{p}/applications"),
		Some(session),
		Some(json!({
			"name": "Whoami", "slug": "whoami", "server_id": server,
			"image": "traefik/whoami:v1", "container_port": 80,
		})),
	)
	.await;
	assert!(s.is_success(), "create application: {s} {a}");
	format!(
		"/api/v1/teams/{team}/projects/{p}/applications/{}/env-vars",
		a["id"].as_i64().unwrap()
	)
}

fn keys(list: &Value) -> Vec<&str> {
	list.as_array()
		.unwrap()
		.iter()
		.map(|v| v["key"].as_str().unwrap())
		.collect()
}

#[sqlx::test]
async fn env_vars_api_bulk_sets_the_good_lines_and_names_the_bad_ones(db: PgPool) {
	let app = app(db.clone()).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let url = env_url(&app, &db, &session, team).await;
	let t = Some(session.as_str());

	let text = "# comment\nONE=1\nnot a pair HUSH\nexport TWO=\"two words\"\n1BAD=x\n";
	let (s, out) = call(
		&app,
		"POST",
		&format!("{url}/bulk"),
		t,
		Some(json!({ "text": text })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{out}");
	assert_eq!(out["saved"], json!(2));
	let lines: Vec<i64> = out["errors"]
		.as_array()
		.unwrap()
		.iter()
		.map(|e| e["line"].as_i64().unwrap())
		.collect();
	assert_eq!(lines, [3, 5]);
	// Neither a value nor a bad line's content comes back.
	assert!(!out.to_string().contains("HUSH") && !out.to_string().contains("two words"));

	// A second push replaces values and leaves keys it does not name alone.
	let (s, _) = call(
		&app,
		"POST",
		&format!("{url}/bulk"),
		t,
		Some(json!({ "text": "TWO=2\nTHREE=3" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (_, list) = call(&app, "GET", &url, t, None).await;
	assert_eq!(keys(&list), ["ONE", "THREE", "TWO"]);
	assert!(!list.to_string().contains("\"value\""), "{list}");
}

#[sqlx::test]
async fn env_vars_api_bulk_needs_the_write_scope(db: PgPool) {
	let app = app(db.clone()).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let url = env_url(&app, &db, &session, team).await;
	let (s, made) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/api-tokens"),
		Some(&session),
		Some(json!({ "name": "ci", "scopes": ["read", "deploy"] })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{made}");
	let token = made["secret"].as_str().unwrap();
	let (s, body) = call(
		&app,
		"POST",
		&format!("{url}/bulk"),
		Some(token),
		Some(json!({ "text": "A=1" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
	assert_eq!(crate::tests::count(&db, "env_vars").await, 0);
}
