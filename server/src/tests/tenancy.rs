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
		&format!("/teams/{a_team}/projects"),
		Some(a_token.as_str()),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert!(s.is_success(), "create project: {s}");
	let a_project = project["id"].as_i64().unwrap();

	let (s, servers) = call(
		&app,
		"GET",
		&format!("/teams/{a_team}/servers"),
		Some(a_token.as_str()),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let a_server = servers[0]["id"].as_i64().unwrap();

	let (s, application) = call(
		&app,
		"POST",
		&format!("/teams/{a_team}/projects/{a_project}/applications"),
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

	let app_path = format!("/teams/{b_team}/projects/{a_project}/applications/{a_app}");

	let (s, _) = call(
		&app,
		"GET",
		&format!("{app_path}/deployments/{a_dep}"),
		Some(b_token.as_str()),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team deployment GET");

	let (s, _) = call(&app, "DELETE", &app_path, Some(b_token.as_str()), None).await;
	assert_eq!(s, StatusCode::NOT_FOUND, "cross-team application DELETE");

	let still_there: bool =
		sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM applications WHERE id = $1)")
			.bind(a_app)
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(still_there, "application row survived cross-team DELETE");
}
