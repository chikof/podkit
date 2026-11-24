//! The Logs page: what it shows, that stored lines are escaped, that the forwarding header is
//! never rendered, and who may change the settings. The agent behind it is the stand-in with a
//! real log store. `cargo test -p podkit-ui --all-features -- log_search`

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
};
use crate::tests::log_search::{StubAgent, add_member, now_ms, sealed_header, stub_agent};
use crate::tests::{count, project_id, seed_app_and_database, state, user_with_team};

const SECRET: &str = "Bearer s3cr3t-token-value";

struct World {
	app: axum::Router,
	cookie: String,
	team: i64,
	agent: StubAgent,
}

async fn world(db: &PgPool) -> World {
	let s = state(db.clone());
	let app = crate::routes(s.clone()).await.unwrap();
	let (cookie, team) = ui_session(&app, "alice").await;
	post_signals(
		&app,
		&format!("/teams/{team}/projects"),
		Some(&cookie),
		&json!({ "name": "shop", "slug": "shop" }),
	)
	.await;
	seed_app_and_database(db, team, project_id(db, "shop").await).await;
	let agent = stub_agent(&s);
	World {
		app,
		cookie,
		team,
		agent,
	}
}

impl World {
	fn url(&self, rest: &str) -> String {
		format!("/teams/{}/logs{rest}", self.team)
	}
}

fn settings(extra: &serde_json::Value) -> serde_json::Value {
	let mut base = json!({
		"retention_days": 7,
		"max_mb": 1024,
		"export_kind": "loki",
		"export_url": "http://loki.example.test/loki/api/v1/push",
		"export_tenant": "",
		"export_authorization": "",
		"clear_authorization": false,
	});
	base.as_object_mut()
		.unwrap()
		.extend(extra.as_object().unwrap().clone());
	base
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn log_search_page_lists_the_search_and_the_settings_with_one_h1(db: PgPool) {
	let w = world(&db).await;
	let (s, _, body) = get_html(&w.app, &w.url(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "<h1>Logs</h1>");
	assert_eq!(body.matches("<h1").count(), 1);
	assert_contains(&body, "Retention and forwarding");
	assert_contains(&body, "Keep lines for (days)");
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn log_search_the_stream_escapes_what_a_container_printed(db: PgPool) {
	let w = world(&db).await;
	w.agent.line(
		"app-5001",
		now_ms() - 2000,
		Some(1),
		None,
		"<script>alert(1)</script> & done",
	);
	let (s, _, body) = get_html(
		&w.app,
		&w.url("/stream?r=app-5001&range=1h"),
		Some(&w.cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_not_contains(&body, "<script>alert(1)");
	assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn log_search_another_teams_page_and_stream_are_not_found(db: PgPool) {
	let w = world(&db).await;
	w.agent
		.line("app-5001", now_ms() - 1000, None, None, "alice's line");
	user_with_team(&w.app, "bob").await;
	let bob = ui_login(&w.app, "bob").await;
	for rest in ["", "/stream?r=app-5001"] {
		let (s, _, body) = get_html(&w.app, &w.url(rest), Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{rest}: {body}");
		assert_not_contains(&body, "alice's line");
	}
	// A Datastar action answers with the error line, not a status; what matters is that nothing
	// was saved.
	let (_, _, body) = post_signals(&w.app, &w.url(""), Some(&bob), &settings(&json!({}))).await;
	assert_not_contains(&body, "Log settings saved");
	assert_eq!(count(&db, "log_settings").await, 0);
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn log_search_a_member_cannot_save_the_settings_with_a_forged_post(db: PgPool) {
	let w = world(&db).await;
	user_with_team(&w.app, "bob").await;
	add_member(&db, w.team, "bob").await;
	let bob = ui_login(&w.app, "bob").await;

	let (s, _, page) = get_html(&w.app, &w.url(""), Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "data-bind:retention_days");

	let (_, _, body) = post_signals(&w.app, &w.url(""), Some(&bob), &settings(&json!({}))).await;
	assert_not_contains(&body, "Log settings saved");
	assert_eq!(count(&db, "log_settings").await, 0);
	assert!(w.agent.configs().is_empty());
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn log_search_the_forwarding_header_is_sealed_and_never_rendered(db: PgPool) {
	let w = world(&db).await;
	let (s, _, body) = post_signals(
		&w.app,
		&w.url(""),
		Some(&w.cookie),
		&settings(&json!({ "export_authorization": SECRET })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_not_contains(&body, SECRET);
	let sealed = sealed_header(&db, w.team)
		.await
		.expect("a header is stored");
	assert_ne!(sealed, SECRET.as_bytes());

	let (s, _, page) = get_html(&w.app, &w.url(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "s3cr3t");
	assert_contains(&page, "Saved. Type a new value to replace it.");
	assert_eq!(w.agent.configs().len(), 1);
}
