//! The presets pages: what a preset asks on its apply page, that an imported one is shown
//! whole and marked, and that saving a project shows what is kept first.

use axum::Router;
use axum::http::StatusCode;
use base64::Engine;
use serde_json::{Value, json};

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};
use crate::tests::{PgPool, app, call, count, db_test, user_with_team};

const BLOG: &str = r#"version = 1

[projects.blog]
name = "Blog"

[projects.blog.environments.production]
name = "Production"

[projects.blog.environments.production.apps.web]
server = "${choose}"
slug = "${project}-web"
image = "ghost:5"
port = 2368

[projects.blog.environments.production.apps.web.env]
MAIL_PASSWORD = "${secret:mail-password}"
SESSION_KEY = "${generate:session-key}"

[projects.blog.environments.production.databases.main]
server = "${choose}"
engine = "postgres"
"#;

struct World {
	app: Router,
	db: PgPool,
	cookie: String,
	token: String,
	team: i64,
	server: String,
}

impl World {
	async fn new(db: PgPool) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "ana").await;
		let cookie = ui_login(&app, "ana").await;
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
			token,
			team,
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}

	fn url(&self, rest: &str) -> String {
		format!("/teams/{}/presets{rest}", self.team)
	}

	async fn page(&self, rest: &str) -> (StatusCode, String) {
		let (status, _, body) = get_html(&self.app, &self.url(rest), Some(&self.cookie)).await;
		(status, body)
	}

	async fn post(&self, rest: &str, signals: &Value) -> (StatusCode, String) {
		let (status, _, body) =
			post_signals(&self.app, &self.url(rest), Some(&self.cookie), signals).await;
		(status, body)
	}

	/// Keeps `BLOG` as a file handed in, and gives its page under `/presets`.
	async fn kept(&self) -> String {
		let (_, kept) = call(
			&self.app,
			"POST",
			&format!("/api/v1/teams/{}/presets", self.team),
			Some(&self.token),
			Some(json!({ "name": "From a friend", "preset": BLOG })),
		)
		.await;
		assert!(kept["id"].is_i64(), "{kept}");
		format!("/kept/{}", kept["id"])
	}

	fn answers(&self) -> Value {
		json!({
			"ps_target": "new", "ps_slug": "my-blog", "ps_name": "My blog", "ps_deploy": false,
			"ps_server_0": self.server, "ps_server_1": self.server, "ps_secret_0": "hunter2",
		})
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn presets_ui_a_file_handed_in_is_kept_marked_and_shown_whole_before_it_is_applied(
	db: PgPool,
) {
	let w = World::new(db).await;
	let (status, list) = w.page("").await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(page_main(&list), "From the catalog");
	assert_contains(page_main(&list), "Keep a preset from a file");

	let file = json!([{
		"name": "blog.toml",
		"contents": base64::engine::general_purpose::STANDARD.encode(BLOG),
	}]);
	let (_, sent) = w
		.post(
			"/keep",
			&json!({ "pk_name": "From a friend", "pk_file": file }),
		)
		.await;
	assert_contains(&sent, &w.url("/kept/"));
	assert_eq!(count(&w.db, "presets").await, 1);

	let (_, list) = w.page("").await;
	assert_contains(page_main(&list), "From a friend");
	assert_contains(page_main(&list), "Unofficial");

	let id: i64 = sqlx::query_scalar("SELECT id FROM presets")
		.fetch_one(&w.db)
		.await
		.unwrap();
	let (status, page) = w.page(&format!("/kept/{id}")).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, "Unofficial");
	assert_contains(main, "This preset came from a file");
	// The whole file is there to be read, and each thing it asks is a field.
	assert_contains(main, "ghost:5");
	assert_contains(main, "production/app/web");
	assert_contains(main, "production/database/main");
	assert_contains(main, "mail-password");
	assert_contains(main, "session-key");
	assert_contains(main, &format!("<option value=\"{}\"", w.server));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn presets_ui_the_form_plans_and_then_makes_the_project(db: PgPool) {
	let w = World::new(db).await;
	let preset = w.kept().await;

	// With a place left unanswered nothing is planned, and the form says which.
	let mut unanswered = w.answers();
	unanswered["ps_server_1"] = json!("");
	let (_, body) = w.post(&format!("{preset}/plan"), &unanswered).await;
	assert_contains(&body, "production/database/main needs a server");

	let (_, body) = w.post(&format!("{preset}/plan"), &w.answers()).await;
	assert_contains(&body, "ghost:5");
	assert_contains(&body, "postgres");
	assert_not_contains(&body, "hunter2");
	assert_eq!(count(&w.db, "projects").await, 0, "a plan makes nothing");

	let (_, body) = w.post(&format!("{preset}/apply"), &w.answers()).await;
	assert_contains(&body, &format!("/teams/{}/projects/", w.team));
	assert_eq!(count(&w.db, "projects").await, 1);
	assert_eq!(count(&w.db, "applications").await, 1);
	let slug: String = sqlx::query_scalar("SELECT slug FROM applications")
		.fetch_one(&w.db)
		.await
		.unwrap();
	assert_eq!(
		slug, "my-blog-web",
		"the application is named after its project"
	);

	// Somebody of another team does not reach the preset.
	let (_, _) = user_with_team(&w.app, "bob").await;
	let bob = ui_login(&w.app, "bob").await;
	let (status, _, body) = get_html(&w.app, &w.url(&preset), Some(&bob)).await;
	assert!(
		status != StatusCode::OK || !body.contains("ghost:5"),
		"{status}"
	);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn presets_ui_saving_a_project_shows_what_is_kept_first(db: PgPool) {
	let w = World::new(db).await;
	let preset = w.kept().await;
	w.post(&format!("{preset}/apply"), &w.answers()).await;
	let project: i64 = sqlx::query_scalar("SELECT id FROM projects")
		.fetch_one(&w.db)
		.await
		.unwrap();

	let (status, page) = w.page(&format!("/save/{project}")).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, "${choose}");
	assert_contains(main, "${project}-web");
	assert_not_contains(main, "hunter2");
	assert_not_contains(main, &format!("server = \"{}\"", w.server));
	assert_eq!(count(&w.db, "presets").await, 1, "looking keeps nothing");

	let (_, sent) = w
		.post(
			&format!("/save/{project}"),
			&json!({ "pv_name": "Blog starter", "pv_description": "Ghost and a database" }),
		)
		.await;
	assert_contains(&sent, &w.url(""));
	assert_eq!(count(&w.db, "presets").await, 2);
	let (_, list) = w.page("").await;
	assert_contains(page_main(&list), "Blog starter");
	assert_contains(page_main(&list), "Ghost and a database");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn presets_ui_the_gallery_of_templates_points_to_presets(db: PgPool) {
	let w = World::new(db).await;
	let (_, _, gallery) = get_html(
		&w.app,
		&format!("/teams/{}/templates", w.team),
		Some(&w.cookie),
	)
	.await;
	assert_contains(page_main(&gallery), &format!("href=\"{}\"", w.url("")));
}
