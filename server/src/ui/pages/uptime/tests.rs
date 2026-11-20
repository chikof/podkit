//! Uptime pages: the list, one check, the settings forms, the public status page and who may do
//! what. `cargo test -p server --all-features -- uptime`

use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use crate::tests::uptime_ui::{add_results, check_id, set_state, state_of};
use crate::tests::{PgPool, app, call, count, db_test, user_with_team};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};

/// alice, her API token, her UI cookie and her team.
async fn alice(app: &axum::Router) -> (String, String, i64) {
	let (token, team) = user_with_team(app, "alice").await;
	let cookie = ui_login(app, "alice").await;
	(token, cookie, team)
}

async fn register(app: &axum::Router, name: &str) {
	let creds = json!({
		"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
	});
	let (s, _) = call(app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(s.is_success());
}

/// alice's team with bob in it as a plain member (who may read, not change).
async fn with_member(app: &axum::Router) -> (String, String, String, i64) {
	let (token, cookie, team) = alice(app).await;
	register(app, "bob").await;
	let (s, _, _) = post_signals(
		app,
		&format!("/teams/{team}/members"),
		Some(&cookie),
		&json!({ "email": "bob@example.com", "newrole": "2" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	(token, cookie, ui_login(app, "bob").await, team)
}

async fn add_check(app: &axum::Router, token: &str, team: i64, body: Value) -> i64 {
	let (s, created) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/uptime/checks"),
		Some(token),
		Some(body),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{created}");
	created["id"].as_i64().unwrap()
}

fn form(name: &str) -> Value {
	json!({
		"name": name, "kind": "http", "url": "https://example.com/health", "method": "GET",
		"expected_status": 200, "keyword": "", "host": "", "port": 443, "timeout_secs": 10,
		"interval_secs": 60, "tls_warn_days": 14, "enabled": true, "status_visible": false,
		"status_name": "",
	})
}

#[db_test]
async fn uptime_page_lists_checks_escaped_and_offers_the_form(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, cookie, team) = alice(&app).await;
	let page = format!("/teams/{team}/uptime");

	let (s, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "No checks yet.");
	assert_contains(main, "Add a check");
	assert_contains(main, "Status page");
	assert_eq!(main.matches("<h1").count(), 1);
	assert_contains(&body, &format!("href=\"/teams/{team}/uptime\""));

	add_check(
		&app,
		&token,
		team,
		json!({ "name": "<b>Shop</b>", "kind": "http", "url": "https://shop.example.com/" }),
	)
	.await;
	let (s, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "&lt;b&gt;Shop&lt;/b&gt;");
	assert_not_contains(main, "<b>Shop</b>");
	assert_contains(main, "shop.example.com");
	assert_contains(main, "Checking");
	assert_eq!(main.matches("<h1").count(), 1);
}

#[db_test]
async fn uptime_the_form_adds_a_check_and_refuses_a_bad_one_in_place(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, cookie, team) = alice(&app).await;
	let page = format!("/teams/{team}/uptime");

	let (s, _, body) = post_signals(&app, &page, Some(&cookie), &form("Docs")).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "location.assign");
	assert_eq!(count(&db, "uptime_checks").await, 1);

	// A password in the URL, a too short interval, a duplicate: nothing is stored.
	let mut login = form("Login");
	login["url"] = json!("https://user:secret@example.com/");
	let (s, _, body) = post_signals(&app, &page, Some(&cookie), &login).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "_error");
	assert_not_contains(&body, "location.assign");
	let mut fast = form("Fast");
	fast["interval_secs"] = json!(5);
	let (_, _, body) = post_signals(&app, &page, Some(&cookie), &fast).await;
	assert_contains(&body, "_error");
	let (_, _, body) = post_signals(&app, &page, Some(&cookie), &form("Docs")).await;
	assert_contains(&body, "already exists");
	assert_eq!(count(&db, "uptime_checks").await, 1);

	// Numbers typed into the fields arrive as text and are accepted.
	let mut tcp = form("Database");
	tcp["kind"] = json!("tcp");
	tcp["host"] = json!("nas");
	tcp["port"] = json!("5432");
	tcp["interval_secs"] = json!("30");
	let (_, _, body) = post_signals(&app, &page, Some(&cookie), &tcp).await;
	assert_contains(&body, "location.assign");
	assert_eq!(count(&db, "uptime_checks").await, 2);
}

#[db_test]
async fn uptime_a_check_page_shows_the_chart_incidents_and_edits_and_deletes(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, cookie, team) = alice(&app).await;
	let id = add_check(&app, &token, team, form("Docs")).await;
	set_state(&db, "Docs", "down").await;
	add_results(&db, "Docs", 0, 4, 3).await;
	let page = format!("/teams/{team}/uptime/{id}");

	let (s, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_eq!(main.matches("<h1").count(), 1);
	assert_contains(main, "https://example.com/health");
	assert_contains(main, "Down");
	assert_contains(main, "chart-latency");
	assert_contains(main, "Response time");
	assert_contains(main, "No incidents");
	assert_contains(main, "Settings");
	// A range link and the chart refresh.
	let (s, _, body) = get_html(&app, &format!("{page}?range=7d"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "last 7 days");
	let (s, _, body) = get_html(&app, &format!("{page}/chart?range=1h"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "chart-latency");

	// Edit: rename and re-point.
	let mut edited = form("Documentation");
	edited["url"] = json!("https://docs.example.com/");
	let (s, _, body) = post_signals(&app, &format!("{page}/edit"), Some(&cookie), &edited).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "location.assign");
	assert_eq!(
		state_of(&db, "Documentation").await,
		"unknown",
		"a new target starts over"
	);
	let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_contains(page_main(&body), "docs.example.com");

	// "Check now" is accepted; delete removes the check and its history.
	let (s, _, _) = post_signals(&app, &format!("{page}/run"), Some(&cookie), &json!({})).await;
	assert_eq!(s, StatusCode::OK);
	let (s, _, body) =
		post_signals(&app, &format!("{page}/delete"), Some(&cookie), &json!({})).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "location.assign");
	assert_eq!(count(&db, "uptime_checks").await, 0);
	assert_eq!(count(&db, "uptime_results").await, 0);
	let (s, _, _) = get_html(&app, &page, Some(&cookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn uptime_a_member_sees_the_checks_but_no_controls_and_forged_posts_change_nothing(
	db: PgPool,
) {
	let app = app(db.clone()).await;
	let (token, _, bob, team) = with_member(&app).await;
	let id = add_check(&app, &token, team, form("Docs")).await;
	let list = format!("/teams/{team}/uptime");
	let page = format!("{list}/{id}");

	let (s, _, body) = get_html(&app, &list, Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "Docs");
	assert_not_contains(main, "Add a check");
	let (s, _, body) = get_html(&app, &page, Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_not_contains(main, "Check now");
	assert_not_contains(main, "delete-check-dialog");
	assert_not_contains(main, "uptime-edit");

	// Forged posts.
	let (_, _, _) = post_signals(&app, &list, Some(&bob), &form("Evil")).await;
	let (_, _, _) = post_signals(&app, &format!("{page}/edit"), Some(&bob), &form("Renamed")).await;
	let (_, _, _) = post_signals(&app, &format!("{page}/run"), Some(&bob), &json!({})).await;
	let (_, _, _) = post_signals(&app, &format!("{page}/delete"), Some(&bob), &json!({})).await;
	let (_, _, _) = post_signals(
		&app,
		&format!("{list}/status"),
		Some(&bob),
		&json!({ "publish": true, "title": "x", "auto_checks": false }),
	)
	.await;
	assert_eq!(count(&db, "uptime_checks").await, 1);
	assert_eq!(check_id(&db, "Docs").await, id);
	let (_, page_json) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/uptime/status-page"),
		Some(&token),
		None,
	)
	.await;
	assert_eq!(page_json["enabled"], false);
}

#[db_test]
async fn uptime_another_teams_pages_are_not_found(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, cookie, team) = alice(&app).await;
	let id = add_check(&app, &token, team, form("Docs")).await;
	let (_, bob_team) = user_with_team(&app, "bob").await;
	let bob = ui_login(&app, "bob").await;

	// Alice's pages as bob: not a member, so not found.
	for uri in [
		format!("/teams/{team}/uptime"),
		format!("/teams/{team}/uptime/{id}"),
		format!("/teams/{team}/uptime/{id}/chart"),
	] {
		let (s, _, body) = get_html(&app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_not_contains(&body, "example.com");
	}
	// Her check through his own team. The page is a 404; the chart refresh is a Datastar action
	// that answers with the error line and no chart.
	let (s, _, _) = get_html(&app, &format!("/teams/{bob_team}/uptime/{id}"), Some(&bob)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{bob_team}/uptime/{id}/chart"),
		Some(&bob),
	)
	.await;
	assert_not_contains(&body, "chart-latency");
	for action in ["edit", "run", "delete"] {
		let uri = format!("/teams/{bob_team}/uptime/{id}/{action}");
		let (_, _, _) = post_signals(&app, &uri, Some(&bob), &form("Stolen")).await;
		let uri = format!("/teams/{team}/uptime/{id}/{action}");
		let (_, _, _) = post_signals(&app, &uri, Some(&bob), &form("Stolen")).await;
	}
	assert_eq!(count(&db, "uptime_checks").await, 1);
	assert_eq!(check_id(&db, "Docs").await, id);
	// Signed out: the sign-in page, never the check.
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/uptime/{id}"), None).await;
	assert_not_contains(&body, "example.com");
	let _ = cookie;
}

#[db_test]
async fn uptime_the_status_page_form_publishes_the_page(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, cookie, team) = alice(&app).await;
	add_check(&app, &token, team, form("Docs")).await;
	let (s, _, _) = get_html(&app, "/status/alice", None).await;
	assert_eq!(s, StatusCode::NOT_FOUND, "off by default");

	let (s, _, body) = post_signals(
		&app,
		&format!("/teams/{team}/uptime/status"),
		Some(&cookie),
		&json!({ "publish": true, "title": "Alice & co", "auto_checks": false }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "location.assign");
	let (s, _, body) = get_html(&app, "/status/alice", None).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "Alice &amp; co");
	let (_, _, list) = get_html(&app, &format!("/teams/{team}/uptime"), Some(&cookie)).await;
	assert_contains(page_main(&list), "href=\"/status/alice\"");
	assert_contains(page_main(&list), "Automatic checks are off");
}

#[db_test]
async fn uptime_the_public_page_shows_only_picked_checks_and_no_internal_address(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, cookie, team) = alice(&app).await;
	let mut shown = form("web-prod-internal");
	shown["url"] = json!("https://web-prod.internal.lan:8443/health?key=hunter2");
	shown["status_visible"] = json!(true);
	shown["status_name"] = json!("<i>Website</i>");
	add_check(&app, &token, team, shown).await;
	let mut hidden = form("secret-db");
	hidden["kind"] = json!("tcp");
	hidden["host"] = json!("db.internal.lan");
	hidden["port"] = json!(5432);
	add_check(&app, &token, team, hidden).await;
	let mut docs = form("docs");
	docs["url"] = json!("https://docs.example.com/");
	docs["status_visible"] = json!(true);
	add_check(&app, &token, team, docs).await;
	set_state(&db, "web-prod-internal", "up").await;
	set_state(&db, "docs", "down").await;
	add_results(&db, "web-prod-internal", 0, 10, 10).await;
	add_results(&db, "web-prod-internal", 3, 10, 9).await;
	let _ = post_signals(
		&app,
		&format!("/teams/{team}/uptime/status"),
		Some(&cookie),
		&json!({ "publish": true, "title": "", "auto_checks": true }),
	)
	.await;

	// No cookie, no token: it is public.
	let (s, headers, body) = get_html(&app, "/status/alice", None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		body.matches("data-check").count(),
		2,
		"the two picked checks"
	);
	assert_contains(&body, "&lt;i&gt;Website&lt;/i&gt;");
	assert_not_contains(&body, "<i>Website</i>");
	assert_contains(&body, "docs");
	assert_contains(&body, "Some systems are down");
	assert_contains(&body, "ui-strip");
	assert_contains(&body, "95%");
	// Nothing internal, however it is spelled.
	for leak in [
		"web-prod",
		"internal",
		".lan",
		"8443",
		"hunter2",
		"secret-db",
		"5432",
		"example.com",
		"example.com/",
	] {
		assert_not_contains(&body, leak);
	}
	// A light page with no script and no request to anyone else.
	assert_not_contains(&body, "<script");
	assert_contains(&body, "class=\"light\"");
	assert_not_contains(&body, "http://");
	assert_not_contains(&body, "https://");
	assert_eq!(body.matches("<h1").count(), 1);
	let cache = headers[header::CACHE_CONTROL].to_str().unwrap();
	assert!(
		cache.contains("public") && cache.contains("max-age"),
		"{cache}"
	);
	assert!(headers.get(header::SET_COOKIE).is_none(), "no cookie");

	// Everything up.
	set_state(&db, "docs", "up").await;
	let (_, _, body) = get_html(&app, "/status/alice", None).await;
	assert_contains(&body, "All systems operational");
}

#[db_test]
async fn uptime_a_team_without_a_status_page_looks_like_a_slug_nobody_has(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, _, team) = alice(&app).await;
	let mut shown = form("docs");
	shown["status_visible"] = json!(true);
	add_check(&app, &token, team, shown).await;
	let (off, _, off_body) = get_html(&app, "/status/alice", None).await;
	let (none, _, none_body) = get_html(&app, "/status/nobody", None).await;
	assert_eq!(off, StatusCode::NOT_FOUND);
	assert_eq!((off, off_body.clone()), (none, none_body));
	assert_not_contains(&off_body, "docs");
}
