//! A team that is not yours does not exist: every route under `/api/v1/teams/{team_id}/`
//! answers 404 to a signed-in caller who is not a member, whatever the method, and whatever
//! the body (a body that does not parse must not turn into a 400, nor a missing permission
//! into a 403: either tells a stranger the team is there).
//!
//! The table is the router itself: its `Debug` output lists every path, so a route added
//! tomorrow is covered the day it exists. `cargo test -p podkit-ui --all-features -- foreign_team_404`

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::testing::{get_html, ui_login};
use crate::tests::{app, call, user_with_team};

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

/// Every path of the router, as it was registered (`/api/v1/teams/{team_id}/…`).
fn paths(router: &axum::Router) -> Vec<String> {
	let debug = format!("{router:?}");
	let mut found: Vec<String> = debug
		.split('"')
		.skip(1)
		.step_by(2)
		.filter(|s| s.starts_with('/'))
		.map(str::to_owned)
		.collect();
	found.sort();
	found.dedup();
	found
}

/// `path` with `{team_id}` replaced by `team` and every other parameter by `1`.
fn fill(path: &str, team: i64) -> String {
	let mut out = String::new();
	let mut rest = path;
	while let Some(open) = rest.find('{') {
		out.push_str(&rest[..open]);
		let close = rest[open..].find('}').map_or(rest.len(), |c| open + c + 1);
		let name = &rest[open + 1..close - 1];
		if name == "team_id" {
			out.push_str(&team.to_string());
		} else {
			out.push('1');
		}
		rest = &rest[close..];
	}
	out.push_str(rest);
	out
}

fn team_routes(all: &[String], prefix: &str) -> Vec<String> {
	all.iter()
		.filter(|p| p.starts_with(prefix) && p.contains("{team_id}"))
		.cloned()
		.collect()
}

/// A full-scope API token of `team`'s member.
async fn api_token(app: &axum::Router, session: &str, team: i64) -> String {
	let (s, body) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/api-tokens"),
		Some(session),
		Some(json!({ "name": "sweep", "scopes": ["read", "write", "deploy", "delete"] })),
	)
	.await;
	assert!(s.is_success(), "make token: {s} {body}");
	body["secret"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn foreign_team_404_every_api_route_of_another_team_is_not_found(db: PgPool) {
	let app = app(db).await;
	let (alice, alice_team) = user_with_team(&app, "alice").await;
	let (bob, bob_team) = user_with_team(&app, "bob").await;
	let bob_token = api_token(&app, &bob, bob_team).await;
	assert_ne!(alice_team, bob_team);

	let all = paths(&app);
	let routes = team_routes(&all, "/api/v1/teams/");
	// The table is read from the router: if it came back nearly empty, the reading broke.
	assert!(
		routes.len() > 150,
		"only {} team routes found",
		routes.len()
	);
	for known in [
		"/api/v1/teams/{team_id}/usage",
		"/api/v1/teams/{team_id}/members",
		"/api/v1/teams/{team_id}/projects/{project_id}/cron/{task_id}/run",
		"/api/v1/teams/{team_id}/projects/{project_id}/applications/{application_id}/deployments",
		"/api/v1/teams/{team_id}/uptime/checks",
	] {
		assert!(
			routes.iter().any(|r| r == known),
			"{known} is not in the table"
		);
	}

	let mut wrong = Vec::new();
	let mut probes = 0;
	for route in &routes {
		let uri = fill(route, alice_team);
		for method in METHODS {
			for (who, token) in [("session", &bob), ("api token", &bob_token)] {
				let body = (method != "GET" && method != "DELETE").then(|| json!({}));
				let (status, _) = call(&app, method, &uri, Some(token), body).await;
				probes += 1;
				if status != StatusCode::NOT_FOUND {
					wrong.push(format!("{method} {route} as a {who}: {status}"));
				}
			}
		}
	}
	assert!(
		wrong.is_empty(),
		"{} of {probes} probes of another team's routes did not say 404:\n{}",
		wrong.len(),
		wrong.join("\n")
	);

	// The same caller is not shut out of their own team, and the owner of the other one is
	// not shut out of theirs.
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{bob_team}/usage"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{alice_team}/usage"),
		Some(&alice),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);

	// A team that does not exist and one that is not yours look the same.
	let (_, ghost) = call(
		&app,
		"GET",
		"/api/v1/teams/987654321/usage",
		Some(&bob),
		None,
	)
	.await;
	let (_, foreign) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{alice_team}/usage"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(ghost, foreign);
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn foreign_team_404_a_caller_who_is_not_signed_in_is_not_told_anything_about_the_team(
	db: PgPool,
) {
	let app = app(db).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/usage"),
		None,
		None,
	)
	.await;
	assert_eq!(
		s,
		StatusCode::BAD_REQUEST,
		"no credentials: the handler's own answer"
	);
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/usage"),
		Some("not-a-session"),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn foreign_team_404_a_member_without_the_permission_is_forbidden_not_hidden(db: PgPool) {
	// The 403 stays for someone inside the team: they know it exists.
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (bob, _) = user_with_team(&app, "bob").await;
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(&alice),
		Some(json!({ "email": "bob@example.com", "role_id": 2 })),
	)
	.await;
	assert!(s.is_success() || s == StatusCode::CONFLICT, "{s}");
	let role: Option<i64> = sqlx::query_scalar(
		"SELECT role_id FROM team_members WHERE team_id = $1 AND user_id = (SELECT id FROM users WHERE name = 'bob')",
	)
	.bind(team)
	.fetch_optional(&db)
	.await
	.unwrap();
	assert!(role.is_some(), "bob joined");
	// A plain member may not create projects.
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&bob),
		Some(json!({ "name": "x", "slug": "x" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn foreign_team_404_the_web_pages_of_another_team_are_not_found_either(db: PgPool) {
	let app = app(db).await;
	let (_, alice_team) = user_with_team(&app, "alice").await;
	let _ = user_with_team(&app, "bob").await;
	let cookie = ui_login(&app, "bob").await;

	let all = paths(&app);
	// Pages only: what the browser opens with a GET, under `/teams/{team_id}/`, that is not
	// the JSON API, an event stream or a socket.
	let pages: Vec<String> = team_routes(&all, "/teams/")
		.into_iter()
		.filter(|p| !p.contains("/events") && !p.ends_with("/ws") && !p.contains("/stream"))
		.collect();
	assert!(pages.len() > 40, "only {} pages found", pages.len());

	let mut wrong = Vec::new();
	for page in &pages {
		let uri = fill(page, alice_team);
		let (status, _, _) = get_html(&app, &uri, Some(&cookie)).await;
		// A route that is not a GET answers 405 (for any team); every page that is one is 404.
		if status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED {
			wrong.push(format!("GET {page}: {status}"));
		}
	}
	assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn foreign_team_404_fill_replaces_the_team_and_every_other_parameter() {
	assert_eq!(
		fill(
			"/api/v1/teams/{team_id}/projects/{project_id}/cron/{task_id}/run",
			7
		),
		"/api/v1/teams/7/projects/1/cron/1/run"
	);
	assert_eq!(fill("/api/v1/teams/{team_id}", 7), "/api/v1/teams/7");
	assert_eq!(fill("/health", 7), "/health");
	let _: Value = json!(null);
}
