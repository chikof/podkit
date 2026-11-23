//! The infrastructure-as-code pages: what they show, what they never show, and who may
//! apply. `cargo test -p server --all-features -- iac_ui`

use axum::Router;
use axum::http::StatusCode;
use base64::Engine;
use serde_json::{Value, json};

use crate::tests::{
	PgPool, app, call, count, db_test, member_role, revoke_permission, user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};

const SECRET: &str = "hunter2-iac-secret-0xCAFE";

fn upload(text: &str) -> Value {
	json!([{
		"name": "podkit.toml",
		"contents": base64::engine::general_purpose::STANDARD.encode(text),
	}])
}

struct World {
	app: Router,
	db: PgPool,
	cookie: String,
	team: i64,
	project: i64,
	server: String,
}

impl World {
	async fn new(db: PgPool, name: &str) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, name).await;
		let cookie = ui_login(&app, name).await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "Shop", "slug": "shop" })),
		)
		.await;
		assert!(s.is_success(), "{s} {project}");
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
			project: project["id"].as_i64().unwrap(),
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}

	fn page(&self, rest: &str) -> String {
		format!("/teams/{}/projects/{}/iac{rest}", self.team, self.project)
	}

	/// A project file with one image app whose env var is a secret.
	fn file(&self, slug: &str) -> String {
		format!(
			r#"version = 1

[projects.shop]
name = "Shop"

[projects.shop.environments.production]
name = "production"

[projects.shop.environments.production.apps.web]
server = "{server}"
slug = "{slug}"
image = "nginx:1"
port = 8080

[projects.shop.environments.production.apps.web.env]
API_KEY = "${{secret:API_KEY}}"
"#,
			server = self.server
		)
	}

	fn signals(file: &str, prune: bool) -> Value {
		json!({
			"iac_file": upload(file),
			"iac_secrets": upload(&format!("API_KEY = \"{SECRET}\"\n")),
			"iac_prune": prune, "iac_deploy": false,
		})
	}

	async fn post(&self, rest: &str, file: &str, prune: bool) -> (StatusCode, String) {
		let (s, _, body) = post_signals(
			&self.app,
			&self.page(rest),
			Some(&self.cookie),
			&Self::signals(file, prune),
		)
		.await;
		(s, body)
	}
}

#[db_test]
async fn iac_ui_the_project_page_exports_a_file_without_secrets(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let (s, applied) = w.post("/apply", &w.file("web"), false).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&applied, SECRET);
	assert_eq!(count(&w.db, "applications").await, 1);

	let (s, _, body) = get_html(&w.app, &w.page(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_eq!(main.matches("<h1").count(), 1);
	assert_contains(main, "Infrastructure as code");
	assert_contains(main, "Export podkit.toml");
	assert_contains(main, "iac/export");
	assert_contains(&body, "iac-apply");
	assert_not_contains(&body, SECRET);

	let (s, headers, file) = get_html(&w.app, &w.page("/export"), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert!(
		headers["content-disposition"]
			.to_str()
			.unwrap()
			.contains("attachment")
	);
	assert_contains(&file, "version = 1");
	assert_contains(&file, "${secret:");
	assert_not_contains(&file, SECRET);
}

#[db_test]
async fn iac_ui_a_plan_shows_the_diff_changes_nothing_and_never_the_secret(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let (s, body) = w.post("/plan", &w.file("web"), false).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "iac-result");
	assert_contains(&body, "web");
	assert_contains(&body, "Create");
	assert_not_contains(&body, SECRET);
	assert_eq!(
		count(&w.db, "applications").await,
		0,
		"a plan changes nothing"
	);

	// A file that does not parse is a message in the error signal (shown as text, never
	// as markup), not a patched element.
	let (_, body) = w
		.post("/plan", "version = <script>alert(1)</script>", false)
		.await;
	assert_contains(&body, "datastar-patch-signals");
	assert_contains(&body, "TOML parse error");
	assert_not_contains(&body, "datastar-patch-elements");

	// No file at all asks for one.
	let (_, _, body) = post_signals(
		&w.app,
		&w.page("/plan"),
		Some(&w.cookie),
		&json!({ "iac_file": [], "iac_secrets": [] }),
	)
	.await;
	assert_contains(&body, "Choose a podkit.toml first.");
}

#[db_test]
async fn iac_ui_apply_twice_is_a_no_op_and_prune_is_off_unless_asked(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let file = w.file("web");
	w.post("/apply", &file, false).await;
	assert_eq!(count(&w.db, "applications").await, 1);
	w.post("/apply", &file, false).await;
	assert_eq!(
		count(&w.db, "applications").await,
		1,
		"the second apply is a no-op"
	);

	// A file that leaves the app out deletes nothing without prune, and does with it.
	let empty = "version = 1\n\n[projects.shop]\nname = \"Shop\"\n\n[projects.shop.environments.production]\nname = \"production\"\n";
	w.post("/apply", empty, false).await;
	assert_eq!(
		count(&w.db, "applications").await,
		1,
		"no prune, nothing deleted"
	);
	w.post("/apply", empty, true).await;
	assert_eq!(count(&w.db, "applications").await, 0);
}

#[db_test]
async fn iac_ui_a_member_who_may_not_apply_has_no_button_and_a_forged_post_does_nothing(
	db: PgPool,
) {
	let w = World::new(db.clone(), "alice").await;
	let role = member_role(&w.db, w.team, "alice").await;
	revoke_permission(&w.db, role, "create", "application").await;
	revoke_permission(&w.db, role, "update", "application").await;

	let (s, _, body) = get_html(&w.app, &w.page(""), Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, "iac-apply");

	let (_, body) = w.post("/apply", &w.file("web"), true).await;
	assert_not_contains(&body, SECRET);
	assert_eq!(
		count(&w.db, "applications").await,
		0,
		"the forged apply made nothing"
	);

	// Planning is still allowed, but offers no apply button.
	let (s, body) = w.post("/plan", &w.file("web"), false).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, "iac-apply");
}

#[db_test]
async fn iac_ui_another_team_gets_404(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let (_, _) = user_with_team(&w.app, "mallory").await;
	let mallory = ui_login(&w.app, "mallory").await;

	for rest in ["", "/export"] {
		let (s, _, _) = get_html(&w.app, &w.page(rest), Some(&mallory)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{rest}");
	}
	for rest in ["/plan", "/apply"] {
		let (_, _, _) = post_signals(
			&w.app,
			&w.page(rest),
			Some(&mallory),
			&World::signals(&w.file("web"), true),
		)
		.await;
	}
	assert_eq!(count(&w.db, "applications").await, 0);
	let (s, _, _) = get_html(&w.app, &w.page(""), None).await;
	assert_ne!(s, StatusCode::OK, "signed out");
}

#[db_test]
async fn iac_ui_the_import_makes_a_new_project_and_escapes_what_the_file_says(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let import = format!("/teams/{}/import", w.team);
	let (s, _, body) = get_html(&w.app, &import, Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(page_main(&body).matches("<h1").count(), 1);
	assert_contains(page_main(&body), "Import projects");

	let file = format!(
		"version = 1\n\n[projects.fresh]\nname = \"<b>Fresh</b>\"\n\n[projects.fresh.environments.production]\nname = \"production\"\n\n[projects.fresh.environments.production.apps.web]\nserver = \"{}\"\nslug = \"fresh-web\"\nimage = \"nginx:1\"\nport = 80\n",
		w.server
	);
	let projects = count(&w.db, "projects").await;
	let (s, _, body) = post_signals(
		&w.app,
		&format!("{import}/plan"),
		Some(&w.cookie),
		&World::signals(&file, false),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "&lt;b&gt;Fresh&lt;/b&gt;");
	assert_not_contains(&body, "<b>Fresh</b>");
	assert_eq!(
		count(&w.db, "projects").await,
		projects,
		"a plan makes nothing"
	);

	let (s, _, _) = post_signals(
		&w.app,
		&format!("{import}/apply"),
		Some(&w.cookie),
		&World::signals(&file, false),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&w.db, "projects").await, projects + 1);
	assert_eq!(count(&w.db, "applications").await, 1);
}

#[db_test]
async fn iac_ui_the_import_is_refused_without_the_right_to_create_projects(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let role = member_role(&w.db, w.team, "alice").await;
	revoke_permission(&w.db, role, "create", "project").await;
	let import = format!("/teams/{}/import", w.team);
	let (s, _, _) = get_html(&w.app, &import, Some(&w.cookie)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let projects = count(&w.db, "projects").await;
	let file = "version = 1\n\n[projects.fresh]\nname = \"Fresh\"\n";
	let (_, _, _) = post_signals(
		&w.app,
		&format!("{import}/apply"),
		Some(&w.cookie),
		&World::signals(file, false),
	)
	.await;
	assert_eq!(count(&w.db, "projects").await, projects);

	// Another team's import page is not found.
	let (_, _) = user_with_team(&w.app, "mallory").await;
	let mallory = ui_login(&w.app, "mallory").await;
	let (s, _, _) = get_html(&w.app, &import, Some(&mallory)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}
