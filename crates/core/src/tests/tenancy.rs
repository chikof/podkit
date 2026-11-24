use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

use super::{app, call, user_with_team};

#[sqlx::test]
async fn other_team_cannot_reach_resources(db: PgPool) {
	let app = app(db.clone()).await;
	let (a_token, a_team) = user_with_team(&app, "alice").await;
	let (b_token, b_team) = user_with_team(&app, "bob").await;

	let (s, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{a_team}/projects"),
		Some(a_token.as_str()),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert!(s.is_success(), "create project: {s}");
	let a_project = project["id"].as_i64().unwrap();

	let (s, servers) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{a_team}/servers"),
		Some(a_token.as_str()),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let a_server = servers[0]["id"].as_i64().unwrap();

	let (s, application) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{a_team}/projects/{a_project}/applications"),
		Some(a_token.as_str()),
		Some(json!({
			"name": "whoami",
			"slug": "whoami",
			"server_id": a_server,
			"repo_url": "https://github.com/traefik/whoami",
			"git_ref": "master",
			"container_port": 80,
		})),
	)
	.await;
	assert!(s.is_success(), "create application: {s} {application}");
	let a_app = application["id"].as_i64().unwrap();

	let a_dep = crypto::generate_id();
	sqlx::query("INSERT INTO deployments (id, application_id) VALUES ($1, $2)")
		.bind(a_dep)
		.bind(a_app)
		.execute(&db)
		.await
		.unwrap();

	let app_path = format!("/api/v1/teams/{b_team}/projects/{a_project}/applications/{a_app}");

	let (s, _) = call(
		&app,
		"GET",
		&format!("{app_path}/deployments/{a_dep}"),
		Some(b_token.as_str()),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team deployment GET");

	let (s, _) = call(
		&app,
		"GET",
		&format!("{app_path}/deployments/{a_dep}/build-logs"),
		Some(b_token.as_str()),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team build-logs SSE");

	let (s, _) = call(
		&app,
		"GET",
		&format!("{app_path}/logs"),
		Some(b_token.as_str()),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team application logs SSE");

	let (s, _) = call(&app, "DELETE", &app_path, Some(b_token.as_str()), None).await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team application DELETE");

	let still_there: bool =
		sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM applications WHERE id = $1)")
			.bind(a_app)
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(still_there, "application row survived cross-team DELETE");

	let a_db = crypto::generate_id();
	sqlx::query(
		"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials)
		VALUES ($1, $2, $3, 'main', 'postgres', '18', ''::bytea)",
	)
	.bind(a_db)
	.bind(a_project)
	.bind(a_server)
	.execute(&db)
	.await
	.unwrap();
	let db_path = format!("/api/v1/teams/{b_team}/projects/{a_project}/databases/{a_db}");

	let (s, _) = call(&app, "GET", &db_path, Some(b_token.as_str()), None).await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team database GET");

	let (s, _) = call(&app, "DELETE", &db_path, Some(b_token.as_str()), None).await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team database DELETE");

	let status: String = sqlx::query_scalar("SELECT status FROM databases WHERE id = $1")
		.bind(a_db)
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(
		status, "pending",
		"database row untouched by cross-team DELETE"
	);
}
