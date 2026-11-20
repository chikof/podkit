//! Registries page: what it shows, what it never shows, and who may change it.
//! `cargo test -p server --all-features -- registries`

use axum::http::StatusCode;
use serde_json::json;

use crate::tests::{PgPool, app, call, count, db_test, registries_ui};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login, ui_session,
};

const PASSWORD: &str = "hunter2-registry-secret";

async fn register(app: &axum::Router, name: &str) {
	let creds = json!({
		"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
	});
	let (s, _) = call(app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(s.is_success());
}

/// alice owns a team and bob joins it as a plain member. Returns (alice, bob, team).
async fn team_with_member(app: &axum::Router) -> (String, String, i64) {
	let (alice, team) = ui_session(app, "alice").await;
	register(app, "bob").await;
	let (s, _, _) = post_signals(
		app,
		&format!("/teams/{team}/members"),
		Some(&alice),
		&json!({ "email": "bob@example.com", "newrole": "2" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	(alice, ui_login(app, "bob").await, team)
}

fn add(name: &str) -> serde_json::Value {
	json!({ "name": name, "url": "ghcr.io", "username": "robot", "password": PASSWORD })
}

#[db_test]
async fn the_page_adds_a_registry_and_never_renders_the_password(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let page = format!("/teams/{team}/registries");

	let (s, _, body) = get_html(&app, &page, Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "No registries yet.");
	assert_contains(&body, "Add a registry");

	let (s, _, body) = post_signals(&app, &page, Some(&alice), &add("<b>GitHub</b>")).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, PASSWORD);
	assert_eq!(count(&db, "registries").await, 1);
	let stored = registries_ui::registry_password(&db).await;
	assert!(
		!String::from_utf8_lossy(&stored).contains(PASSWORD),
		"the password is stored encrypted"
	);

	let (s, _, body) = get_html(&app, &page, Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, PASSWORD);
	let main = page_main(&body);
	assert_contains(main, "&lt;b&gt;GitHub&lt;/b&gt;");
	assert_not_contains(main, "<b>GitHub</b>");
	assert_contains(main, "ghcr.io");
	assert_contains(main, "robot");
	assert_eq!(main.matches("<h1").count(), 1);
}

#[db_test]
async fn a_member_cannot_add_edit_or_delete_and_forged_posts_are_refused(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, bob, team) = team_with_member(&app).await;
	let page = format!("/teams/{team}/registries");
	let (s, _, _) = post_signals(&app, &page, Some(&alice), &add("GitHub")).await;
	assert_eq!(s, StatusCode::OK);
	let id = registries_ui::registry_id(&db).await;

	// Bob may read: he sees the registry but none of the controls.
	let (s, _, body) = get_html(&app, &page, Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "ghcr.io");
	assert_not_contains(main, "Add a registry");
	assert_not_contains(main, "Test login");
	assert_not_contains(main, "delete-registry-dialog");
	assert_not_contains(main, PASSWORD);

	// Forged posts change nothing.
	let (_, _, _) = post_signals(&app, &page, Some(&bob), &add("Evil")).await;
	let (_, _, _) = post_signals(
		&app,
		&format!("{page}/{id}/edit"),
		Some(&bob),
		&json!({ "ed_name": "Renamed", "ed_username": "x", "ed_password": "" }),
	)
	.await;
	let (_, _, _) =
		post_signals(&app, &format!("{page}/{id}/delete"), Some(&bob), &json!({})).await;
	assert_eq!(count(&db, "registries").await, 1);
	let name = registries_ui::registry_name(&db).await;
	assert_eq!(name, "GitHub");
}

#[db_test]
async fn another_teams_registries_are_not_found(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	register(&app, "mallory").await;
	let mallory = ui_login(&app, "mallory").await;
	let page = format!("/teams/{team}/registries");
	let (s, _, _) = post_signals(&app, &page, Some(&alice), &add("GitHub")).await;
	assert_eq!(s, StatusCode::OK);
	let id = registries_ui::registry_id(&db).await;

	let (s, _, body) = get_html(&app, &page, Some(&mallory)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&body, "ghcr.io");
	let (_, _, _) = post_signals(
		&app,
		&format!("{page}/{id}/delete"),
		Some(&mallory),
		&json!({}),
	)
	.await;
	assert_eq!(count(&db, "registries").await, 1);
}
