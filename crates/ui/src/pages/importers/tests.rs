//! Settings → Import: what the page shows, what it never shows, and who may use it.
//! `cargo test -p podkit-ui --all-features -- importers_ui`

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};
use crate::tests::importers::{COOLIFY, DOKPLOY, serve};
use crate::tests::{
	PgPool, app, call, count, db_test, member_role, revoke_permission, user_with_team,
};
use podkit_core::importers::Source;

const SECRET: &str = "coolify-secret-DB-9f3a";

struct World {
	app: Router,
	db: PgPool,
	cookie: String,
	team: i64,
	server: String,
}

impl World {
	async fn new(db: PgPool, name: &str) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, name).await;
		let cookie = ui_login(&app, name).await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		Self {
			app,
			db,
			cookie,
			team,
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}

	fn page(&self, rest: &str) -> String {
		format!("/teams/{}/settings/import{rest}", self.team)
	}

	fn signals(&self, source: &str, url: &str, token: &str, id: &str) -> Value {
		json!({
			"imp_source": source, "imp_url": url, "imp_token": token,
			"imp_server": self.server, "imp_deploy": false, "imp_id": id,
		})
	}

	async fn post(&self, rest: &str, signals: &Value) -> (StatusCode, String) {
		let (s, _, body) =
			post_signals(&self.app, &self.page(rest), Some(&self.cookie), signals).await;
		(s, body)
	}
}

/// The preview id a plan's response put in the `imp_id` signal.
fn preview_id(body: &str) -> String {
	let at = body.find("\"imp_id\":\"").expect("the plan sets imp_id") + "\"imp_id\":\"".len();
	body[at..]
		.chars()
		.take_while(char::is_ascii_hexdigit)
		.collect()
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn importers_ui_the_page_has_one_h1_a_form_and_no_credential(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (s, _, body) = get_html(&w.app, &w.page(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_eq!(main.matches("<h1").count(), 1);
	for label in [
		"Coolify",
		"Dokploy",
		"CapRover",
		"Address",
		"Credential",
		"Read and plan",
	] {
		assert_contains(main, label);
	}
	assert_contains(main, "type=\"password\"");
	assert_contains(main, "settings/import/plan");
	// The sidebar of the settings lists it.
	assert_contains(&body, &format!("/teams/{}/settings/import", w.team));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn importers_ui_a_plan_shows_the_report_and_the_plan_and_changes_nothing(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Coolify, COOLIFY).await;
	let (s, body) = w
		.post("/plan", &w.signals("coolify", &url, "right-token", ""))
		.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_contains(&body, "import-result");
	assert_contains(&body, "What was read");
	assert_contains(&body, "Not supported");
	assert_contains(&body, "custom labels are not supported");
	assert_contains(&body, "What would be made");
	assert_contains(&body, "Import\u{2026}");
	// The credential is cleared from the form, a secret value is on no page.
	assert_contains(&body, "\"imp_token\":\"\"");
	assert_not_contains(&body, "right-token");
	assert_not_contains(&body, SECRET);
	assert_eq!(count(&w.db, "projects").await, 0);
	assert_eq!(count(&w.db, "applications").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn importers_ui_apply_makes_the_projects_once(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Dokploy, DOKPLOY).await;
	let (_, planned) = w
		.post("/plan", &w.signals("dokploy", &url, "right-token", ""))
		.await;
	let id = preview_id(&planned);
	assert_eq!(id.len(), 32, "{planned}");

	let (s, done) = w.post("/apply", &w.signals("dokploy", "", "", &id)).await;
	assert_eq!(s, StatusCode::OK, "{done}");
	assert_not_contains(&done, "dokploy-secret");
	assert_eq!(count(&w.db, "projects").await, 1);
	assert!(count(&w.db, "applications").await >= 1);

	// The preview is used up.
	let before = count(&w.db, "applications").await;
	let (_, again) = w.post("/apply", &w.signals("dokploy", "", "", &id)).await;
	assert_contains(&again, "That no longer exists.");
	assert_eq!(count(&w.db, "applications").await, before);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn importers_ui_a_wrong_credential_is_a_message_not_an_element(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Coolify, COOLIFY).await;
	let (_, body) = w
		.post("/plan", &w.signals("coolify", &url, "wrong-token", ""))
		.await;
	assert_contains(&body, "datastar-patch-signals");
	assert_not_contains(&body, "datastar-patch-elements");
	assert_not_contains(&body, "wrong-token");

	let (_, body) = w.post("/plan", &w.signals("heroku", &url, "x", "")).await;
	assert_contains(&body, "Choose the platform to import from.");
	let (_, body) = w
		.post(
			"/plan",
			&w.signals("coolify", "<script>alert(1)</script>", "x", ""),
		)
		.await;
	assert_not_contains(&body, "<script>alert(1)</script>");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn importers_ui_needs_create_project_and_a_forged_post_is_refused(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (url, _) = serve(Source::Coolify, COOLIFY).await;
	let role = member_role(&w.db, w.team, "alice").await;
	revoke_permission(&w.db, role, "create", "project").await;

	let (s, _, body) = get_html(&w.app, &w.page(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
	assert_not_contains(&body, "Read and plan");
	let (_, body) = w
		.post("/plan", &w.signals("coolify", &url, "right-token", ""))
		.await;
	assert_not_contains(&body, "datastar-patch-elements");
	assert_not_contains(&body, "What was read");
	let (_, _) = w.post("/apply", &w.signals("coolify", "", "", "abc")).await;
	assert_eq!(count(&w.db, "projects").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn importers_ui_another_team_gets_nothing_of_a_preview(db: PgPool) {
	let a = World::new(db.clone(), "alice").await;
	let b = World::new(db.clone(), "bob").await;
	let (url, _) = serve(Source::Coolify, COOLIFY).await;
	let (_, planned) = a
		.post("/plan", &a.signals("coolify", &url, "right-token", ""))
		.await;
	let id = preview_id(&planned);

	// Bob in his own team, then Bob at Alice's address.
	let (_, body) = b.post("/apply", &b.signals("coolify", "", "", &id)).await;
	assert_contains(&body, "That no longer exists.");
	let (s, _, body) = post_signals(
		&b.app,
		&a.page("/apply"),
		Some(&b.cookie),
		&b.signals("coolify", "", "", &id),
	)
	.await;
	// A forged POST at Alice's address is refused, or answers as Bob's own team would
	// (the preview is not his): nothing is made and nothing of Alice's preview comes back.
	assert!(
		s == StatusCode::NOT_FOUND
			|| s == StatusCode::FORBIDDEN
			|| body.contains("That no longer exists."),
		"{s} {body}"
	);
	assert_not_contains(&body, "What was read");
	assert_eq!(count(&db, "projects").await, 0);
}
