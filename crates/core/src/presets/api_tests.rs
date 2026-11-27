//! API tests of presets: what one asks, and applying it into a new and an existing project.

use serde_json::{Value, json};

use axum::http::StatusCode;

use crate::tests::{PgPool, app, call, count, db_test, user_with_team};

/// A preset as it is saved: no server chosen, one secret to type and one to make up.
const BLOG: &str = r#"version = 1

[projects.blog]
name = "Blog"

[projects.blog.environments.production]
name = "Production"

[projects.blog.environments.production.apps.web]
server = "${choose}"
slug = "blog-web"
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
	app: axum::Router,
	db: PgPool,
	token: String,
	team: i64,
	server: String,
}

impl World {
	async fn new(db: PgPool) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "ana").await;
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
			token,
			team,
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}

	async fn post(&self, rest: &str, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"POST",
			&format!("/api/v1/teams/{}/presets/{rest}", self.team),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	/// The project `id` as the team's list has it.
	async fn project(&self, id: i64) -> Value {
		let (_, projects) = call(
			&self.app,
			"GET",
			&format!("/api/v1/teams/{}/projects", self.team),
			Some(&self.token),
			None,
		)
		.await;
		let found = projects.as_array().unwrap().iter().find(|p| p["id"] == id);
		found
			.cloned()
			.unwrap_or_else(|| panic!("no project {id} in {projects}"))
	}

	fn answers(&self) -> Value {
		json!({
			"servers": {
				"production/app/web": self.server,
				"production/database/main": self.server,
			},
			"secrets": { "mail-password": "hunter2" },
		})
	}
}

#[db_test]
async fn a_preset_says_what_it_asks_and_what_the_team_has_to_answer_with(db: PgPool) {
	let w = World::new(db).await;
	let (status, asked) = w.post("needs", json!({ "preset": BLOG })).await;
	assert_eq!(status, StatusCode::OK, "{asked}");
	assert_eq!(asked["slug"], "blog");
	assert_eq!(asked["name"], "Blog");
	let slots: Vec<&str> = asked["needs"]["servers"]
		.as_array()
		.unwrap()
		.iter()
		.filter_map(|s| s["at"].as_str())
		.collect();
	assert_eq!(slots, ["production/app/web", "production/database/main"]);
	assert_eq!(asked["needs"]["secrets"], json!(["mail-password"]));
	assert_eq!(asked["needs"]["generated"], json!(["session-key"]));
	assert_eq!(asked["available"]["servers"], json!([w.server]));

	let (status, body) = w.post("needs", json!({ "preset": "version = 1\n" })).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[db_test]
async fn a_preset_becomes_a_new_project_with_the_answers_in_place(db: PgPool) {
	let w = World::new(db).await;
	let request = json!({
		"preset": BLOG,
		"answers": w.answers(),
		"target": { "new": { "slug": "my-blog", "name": "My blog" } },
	});

	// Left unanswered, everything missing is named at once and nothing is made.
	let mut unanswered = request.clone();
	unanswered["answers"] = json!({});
	let (status, body) = w.post("apply", unanswered).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	let message = body["error"].as_str().unwrap();
	for need in [
		"production/app/web needs a server",
		"production/database/main needs a server",
		"the secret mail-password needs a value",
	] {
		assert!(message.contains(need), "{need} is not in: {message}");
	}
	assert_eq!(count(&w.db, "applications").await, 0);

	let (status, plan) = w.post("plan", request.clone()).await;
	assert_eq!(status, StatusCode::OK, "{plan}");
	assert_eq!(plan["issues"], json!([]), "{plan}");
	assert_eq!(
		count(&w.db, "applications").await,
		0,
		"a plan makes nothing"
	);

	let (status, done) = w.post("apply", request).await;
	assert_eq!(status, StatusCode::OK, "{done}");
	let project = done["done"]["project_id"].as_i64().unwrap();
	let made = w.project(project).await;
	assert_eq!(made["slug"], "my-blog");
	assert_eq!(made["name"], "My blog");
	assert_eq!(count(&w.db, "applications").await, 1);
	assert_eq!(count(&w.db, "databases").await, 1);

	// Both secrets are set, the typed one and the made-up one, and the file names neither value.
	let (_, exported) = call(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{}/projects/{project}/export", w.team),
		Some(&w.token),
		None,
	)
	.await;
	let exported = exported.to_string();
	assert!(!exported.contains("hunter2"));
	let variables: i64 = sqlx::query_scalar("SELECT count(*) FROM env_vars")
		.fetch_one(&w.db)
		.await
		.unwrap();
	assert_eq!(variables, 2);

	// The file as written fixes its application's slug, which is unique on a server: a
	// second project from it on the same server is refused before anything is made.
	let (status, body) = w
		.post(
			"apply",
			json!({
				"preset": BLOG,
				"answers": w.answers(),
				"target": { "new": { "slug": "other-blog" } },
			}),
		)
		.await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
	let issues = body["plan"]["issues"].to_string();
	assert!(
		issues.contains("blog-web") && issues.contains("already runs"),
		"{issues}"
	);
	assert_eq!(count(&w.db, "applications").await, 1);
	assert_eq!(count(&w.db, "databases").await, 1);
	assert_eq!(
		count(&w.db, "projects").await,
		1,
		"no empty project is left: {status} {body}"
	);
}

#[db_test]
async fn a_preset_is_added_to_a_project_that_exists_and_takes_nothing_from_it(db: PgPool) {
	let w = World::new(db).await;
	let (_, shop) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/projects", w.team),
		Some(&w.token),
		Some(json!({ "name": "Shop", "slug": "shop" })),
	)
	.await;
	let shop_id = shop["id"].as_i64().unwrap();
	let (status, _) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/projects/{shop_id}/applications", w.team),
		Some(&w.token),
		Some(json!({
			"name": "Storefront", "slug": "storefront", "image": "nginx:1", "port": 80,
			"server_id": null,
		})),
	)
	.await;
	let had = count(&w.db, "applications").await;
	assert!(status.is_success() || had == 0, "{status}");

	let (status, done) = w
		.post(
			"apply",
			json!({ "preset": BLOG, "answers": w.answers(), "target": { "project": shop_id } }),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{done}");
	assert_eq!(done["done"]["project_id"], shop_id);
	assert_eq!(count(&w.db, "applications").await, had + 1);
	assert_eq!(count(&w.db, "projects").await, 1, "no project is made");
	let after = w.project(shop_id).await;
	assert_eq!(after["name"], "Shop", "the preset does not rename it");
}

#[db_test]
async fn a_preset_naming_a_server_the_team_does_not_have_is_refused_by_the_plan(db: PgPool) {
	let w = World::new(db).await;
	let mut answers = w.answers();
	answers["servers"]["production/database/main"] = json!("no-such-server");
	let (status, body) = w
		.post(
			"apply",
			json!({ "preset": BLOG, "answers": answers, "target": { "new": {} } }),
		)
		.await;
	assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
	assert!(
		body["plan"]["issues"]
			.to_string()
			.contains("no-such-server"),
		"{body}"
	);
	assert_eq!(count(&w.db, "projects").await, 0);
}

#[db_test]
async fn a_project_is_saved_as_a_preset_without_the_team_s_names_and_applies_elsewhere(db: PgPool) {
	let w = World::new(db).await;
	let (_, done) = w
		.post(
			"apply",
			json!({ "preset": BLOG, "answers": w.answers(), "target": { "new": {} } }),
		)
		.await;
	let project = done["done"]["project_id"].as_i64().unwrap();
	let presets = format!("/api/v1/teams/{}/presets", w.team);

	// What would be kept is shown first, and keeps nothing.
	let (status, shown) = call(
		&w.app,
		"GET",
		&format!("{presets}/from-project/{project}"),
		Some(&w.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{shown}");
	let file = shown["preset"].as_str().unwrap();
	assert!(!file.contains(&format!("\"{}\"", w.server)), "{file}");
	assert!(file.contains("${choose}"));
	assert!(file.contains("slug = \"${project}-web\""), "{file}");
	assert!(!file.contains("hunter2"));
	assert_eq!(count(&w.db, "presets").await, 0);

	let (status, kept) = call(
		&w.app,
		"POST",
		&format!("{presets}/from-project/{project}"),
		Some(&w.token),
		Some(json!({ "name": "Blog starter", "description": "Ghost and a database" })),
	)
	.await;
	assert_eq!(status, StatusCode::CREATED, "{kept}");
	assert_eq!(kept["origin"], "saved");
	assert_eq!(kept["slug"], "blog");

	// The list has it without its file; one preset has the file, which is the one shown.
	let (_, list) = call(&w.app, "GET", &presets, Some(&w.token), None).await;
	assert_eq!(list.as_array().map(Vec::len), Some(1), "{list}");
	assert_eq!(list[0]["name"], "Blog starter");
	assert!(list[0].get("file").is_none(), "{list}");
	let (_, one) = call(
		&w.app,
		"GET",
		&format!("{presets}/{}", kept["id"]),
		Some(&w.token),
		None,
	)
	.await;
	assert_eq!(one["file"], file);

	// The kept file asks for its servers again, and applies as a second project.
	let (_, asked) = w.post("needs", json!({ "preset": file })).await;
	assert_eq!(asked["needs"]["servers"].as_array().unwrap().len(), 2);
	assert!(asked["needs"]["servers"][0]["suggested"].is_null());
	let mut secrets = serde_json::Map::new();
	for name in asked["needs"]["secrets"].as_array().unwrap() {
		secrets.insert(name.as_str().unwrap().to_owned(), json!("typed"));
	}
	let (status, again) = w
		.post(
			"apply",
			json!({
				"preset": file,
				"answers": { "servers": w.answers()["servers"], "secrets": secrets },
				"target": { "new": { "slug": "blog-two", "name": "Blog two" } },
			}),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "{again}");
	assert_eq!(count(&w.db, "projects").await, 2);
	assert_eq!(count(&w.db, "applications").await, 2);

	// A second of the same slug is refused; removing one leaves the projects alone.
	let (status, _) = call(
		&w.app,
		"POST",
		&format!("{presets}/from-project/{project}"),
		Some(&w.token),
		Some(json!({ "name": "Again" })),
	)
	.await;
	assert_eq!(status, StatusCode::CONFLICT);
	let (status, _) = call(
		&w.app,
		"DELETE",
		&format!("{presets}/{}", kept["id"]),
		Some(&w.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	assert_eq!(count(&w.db, "presets").await, 0);
	assert_eq!(count(&w.db, "projects").await, 2);
}

#[db_test]
async fn a_preset_handed_in_as_a_file_is_kept_as_imported_and_only_for_its_team(db: PgPool) {
	let w = World::new(db).await;
	let presets = format!("/api/v1/teams/{}/presets", w.team);
	let (status, kept) = call(
		&w.app,
		"POST",
		&presets,
		Some(&w.token),
		Some(json!({ "name": "From a friend", "preset": BLOG })),
	)
	.await;
	assert_eq!(status, StatusCode::CREATED, "{kept}");
	assert_eq!(kept["origin"], "imported");

	for (what, body) in [
		(
			"not a project file",
			json!({ "name": "X", "preset": "nonsense = [" }),
		),
		(
			"no project in it",
			json!({ "name": "X", "preset": "version = 1\n" }),
		),
		("no name", json!({ "name": " ", "preset": BLOG })),
		(
			"a slug that is not one",
			json!({ "name": "X", "slug": "Not A Slug", "preset": BLOG }),
		),
	] {
		let (status, answer) = call(&w.app, "POST", &presets, Some(&w.token), Some(body)).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {answer}");
	}

	// Another team sees and reaches none of it.
	let (bob, bobs) = user_with_team(&w.app, "bob").await;
	let (_, list) = call(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{bobs}/presets"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(list, json!([]));
	for (method, path) in [
		("GET", format!("{presets}/{}", kept["id"])),
		("DELETE", format!("{presets}/{}", kept["id"])),
		(
			"GET",
			format!("/api/v1/teams/{bobs}/presets/{}", kept["id"]),
		),
	] {
		let (status, _) = call(&w.app, method, &path, Some(&bob), None).await;
		assert!(status.is_client_error(), "{method} {path}: {status}");
	}
	assert_eq!(count(&w.db, "presets").await, 1);
}
