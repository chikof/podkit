//! Canvas page tests: the page and its list view, the confirm popover and the wires behind it,
//! escaping, forged posts, the other team's 404s, and the live status.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::tests::{
	PgPool, call, count, db_test, member_role, revoke_permission, seed_deployment, seed_model,
	state, user_with_team,
};
use crate::ui::testing::{self, assert_contains, assert_not_contains, page_main, ui_login};

struct Ui {
	app: Router,
	cookie: String,
	token: String,
	db: PgPool,
	team: i64,
	project: i64,
	server: i64,
}

fn enc(s: &str) -> String {
	use std::fmt::Write as _;
	s.bytes().fold(String::new(), |mut out, b| {
		if b.is_ascii_alphanumeric() {
			out.push(b as char);
		} else {
			let _ = write!(out, "%{b:02X}");
		}
		out
	})
}

impl Ui {
	async fn new(db: PgPool) -> Self {
		let app = crate::routes::routes(state(db.clone())).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let cookie = ui_login(&app, "alice").await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let (_, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "shop", "slug": "shop" })),
		)
		.await;
		Self {
			app,
			cookie,
			token,
			db,
			team,
			project: project["id"].as_i64().unwrap(),
			server: servers[0]["id"].as_i64().unwrap(),
		}
	}

	fn url(&self, tail: &str) -> String {
		format!(
			"/teams/{}/projects/{}/canvas{tail}",
			self.team, self.project
		)
	}

	async fn api(&self, rest: &str, body: Value) -> i64 {
		let (s, v) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/{rest}",
				self.team, self.project
			),
			Some(&self.token),
			Some(body),
		)
		.await;
		assert!(s.is_success(), "{rest}: {s} {v}");
		v["id"].as_i64().unwrap()
	}

	async fn application(&self, name: &str) -> i64 {
		let slug = name
			.to_lowercase()
			.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
			.trim_matches('-')
			.to_owned();
		self.api(
			"applications",
			json!({
				"name": name, "slug": slug, "server_id": self.server,
				"repo_url": "https://github.com/acme/x", "git_ref": "main", "container_port": 3000,
			}),
		)
		.await
	}

	async fn database(&self, name: &str) -> i64 {
		self.api(
			"databases",
			json!({ "name": name, "engine": "postgres", "server_id": self.server }),
		)
		.await
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = testing::get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	/// A Datastar GET: the signals ride in the `datastar` query parameter.
	async fn action_get(&self, tail: &str, signals: &Value) -> (StatusCode, String) {
		self.get(&format!(
			"{}?datastar={}",
			self.url(tail),
			enc(&signals.to_string())
		))
		.await
	}

	async fn post(&self, tail: &str, signals: &Value) -> (StatusCode, String) {
		let (s, _, body) =
			testing::post_signals(&self.app, &self.url(tail), Some(&self.cookie), signals).await;
		(s, body)
	}
}

#[db_test]
async fn canvas_page_draws_the_nodes_the_list_and_the_keyboard_model(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	let (status, html) = ui.get(&ui.url("")).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(html.matches("<h1").count(), 1, "one h1");
	let main = page_main(&html);
	// One node per resource, with the ids the script and the server agree on.
	assert_contains(main, &format!("id=\"n-a-{web}\""));
	assert_contains(main, &format!("id=\"n-d-{pg}\""));
	assert_eq!(main.matches("class=\"ui-cv-node\"").count(), 2);
	// Each node says what it may be wired to, decided by the server.
	assert_contains(main, &format!("data-targets=\"d-{pg}\""));
	assert_contains(main, &format!("data-targets=\"a-{web}\""));
	// The canvas is a labelled group with the key hint; the list view carries the same facts.
	assert_contains(main, "role=\"group\"");
	assert_contains(main, "aria-describedby=\"cv-keys\"");
	assert_contains(main, "id=\"cv-live\"");
	assert_contains(main, "Resources in production and how they are connected");
	assert_contains(main, "Connect…");
	assert_contains(main, "Project map");
	assert_contains(main, "/static/canvas.js?v=");
	assert_contains(main, "data-wire");
	// The environment page leads here.
	let (_, env_page) = ui
		.get(&format!("/teams/{}/projects/{}", ui.team, ui.project))
		.await;
	assert_contains(&env_page, &ui.url(""));
}

#[db_test]
async fn canvas_escapes_every_name(db: PgPool) {
	let ui = Ui::new(db).await;
	ui.application("<img src=x onerror=alert(1)>").await;
	ui.database("<b>main</b>").await;
	let (_, html) = ui.get(&ui.url("")).await;
	assert_contains(&html, "&lt;img src=x onerror=alert(1)&gt;");
	assert_contains(&html, "&lt;b&gt;main&lt;/b&gt;");
	assert_not_contains(&html, "<img src=x");
	assert_not_contains(&html, "<b>main</b>");
	// The same names in an attribute (data-name, aria-label) cannot leave it.
	assert_not_contains(&html, "data-name=\"<");
}

#[db_test]
async fn canvas_an_empty_project_shows_the_empty_state(db: PgPool) {
	let ui = Ui::new(db).await;
	let (status, html) = ui.get(&ui.url("")).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&html);
	assert_contains(main, "Nothing to connect yet");
	assert_contains(main, "Create resource");
	assert_not_contains(main, "id=\"cv-view\"");
	assert_not_contains(main, "canvas.js");
}

#[db_test]
async fn canvas_the_confirm_popover_lists_the_exact_changes_and_no_secret(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	seed_deployment(&ui.db, web, "running").await;
	let signals = json!({ "cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}") });
	let (status, body) = ui.action_get("/wire/preview", &signals).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "Connect web to main");
	assert_contains(&body, "<code>DATABASE_URL</code>");
	assert_contains(&body, "Rolling redeploy");
	assert_contains(&body, "Connect and redeploy");
	assert_contains(&body, "Nothing changes until you confirm");
	assert_contains(&body, "showPopover");
	// Whatever the credentials are, nothing of them is drawn.
	assert_not_contains(&body, "postgres://");
	assert_not_contains(&body, "podkit-db-");
	assert_eq!(
		count(&ui.db, "env_vars").await,
		0,
		"a preview changes nothing"
	);

	// Dropped the other way round it is the same wire.
	let flipped = json!({ "cvFrom": format!("d-{pg}"), "cvTo": format!("a-{web}") });
	let (_, again) = ui.action_get("/wire/preview", &flipped).await;
	assert_contains(&again, "Connect web to main");
}

#[db_test]
async fn canvas_a_refused_wire_is_a_toast_with_the_reason(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let signals = json!({ "cvFrom": format!("a-{web}"), "cvTo": format!("a-{web}") });
	let (status, body) = ui.action_get("/wire/preview", &signals).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "Pick a different resource");
	assert_not_contains(&body, "cv-pop");
}

#[db_test]
async fn canvas_confirming_applies_the_wire_and_redraws_the_map(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	seed_deployment(&ui.db, web, "running").await;
	let signals = json!({ "cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}") });
	let (status, body) = ui.post("/wire", &signals).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "web linked to main. Redeploying.");
	assert_contains(&body, "hidePopover");
	assert_contains(&body, "id=\"cv-world\"");
	assert_contains(&body, "data-label=\"DATABASE_URL\"");
	assert_not_contains(&body, "postgres://");
	assert_eq!(count(&ui.db, "env_vars").await, 1);

	// The page now has the link in the canvas and in the list.
	let (_, html) = ui.get(&ui.url("")).await;
	let main = page_main(&html);
	assert_contains(main, "data-kind=\"env\"");
	assert_contains(main, "DATABASE_URL variable: web to main");
	assert_contains(main, "ui-cv-links");
	assert_contains(main, "id=\"cv-poll\"");
}

#[db_test]
async fn canvas_unlinking_asks_first_then_removes_the_variable(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	ui.post(
		"/wire",
		&json!({ "cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}") }),
	)
	.await;
	let signals = json!({
		"cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}"),
		"cvKind": "env", "cvLabel": "DATABASE_URL",
	});
	let (_, preview) = ui.action_get("/unlink/preview", &signals).await;
	assert_contains(&preview, "Unlink web from main");
	assert_contains(
		&preview,
		"Remove <code>DATABASE_URL</code> from <code>web</code>",
	);
	assert_contains(&preview, "autofocus");
	assert_eq!(
		count(&ui.db, "env_vars").await,
		1,
		"a preview changes nothing"
	);

	let (_, done) = ui.post("/unlink", &signals).await;
	assert_contains(&done, "web unlinked from main.");
	assert_eq!(count(&ui.db, "env_vars").await, 0);

	// A link that is no longer there says so instead of pretending.
	let (_, again) = ui.post("/unlink", &signals).await;
	assert_contains(&again, "_error");
	assert_not_contains(&again, "unlinked from");
}

#[db_test]
async fn canvas_the_list_view_picks_a_target_in_the_popover(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	let signals = json!({ "cvFrom": format!("a-{web}"), "cvTo": "", "cvPick": true });
	let (_, body) = ui.action_get("/wire/preview", &signals).await;
	assert_contains(&body, "Connect to");
	assert_contains(
		&body,
		&format!("<option value=\"d-{pg}\" selected>main</option>"),
	);
	assert_contains(&body, "Connect web to main");
}

#[db_test]
async fn canvas_forged_posts_change_nothing(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	let role = member_role(&ui.db, ui.team, "alice").await;
	let good = json!({ "cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}") });

	// Without the permission to write env vars: the message, and no change at all.
	revoke_permission(&ui.db, role, "create", "env_var").await;
	let (status, body) = ui.post("/wire", &good).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "permission");
	assert_eq!(count(&ui.db, "env_vars").await, 0);

	// Garbage keys, an unknown kind and ids nobody has are all refused.
	for bad in [
		json!({ "cvFrom": "x-1", "cvTo": "d-1" }),
		json!({ "cvFrom": "a-1; drop table", "cvTo": "d-1" }),
		json!({ "cvFrom": "a-999999", "cvTo": "d-999999" }),
		json!({ "cvFrom": format!("a-{web}"), "cvTo": "" }),
		json!({}),
	] {
		let (status, body) = ui.post("/wire", &bad).await;
		assert_eq!(status, StatusCode::OK, "{bad}");
		assert_contains(&body, "_error");
	}
	for kind in ["", "x", "network"] {
		let (_, body) = ui
			.post(
				"/unlink",
				&json!({
					"cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}"),
					"cvKind": kind, "cvLabel": "DATABASE_URL",
				}),
			)
			.await;
		assert_contains(&body, "_error");
	}
	assert_eq!(count(&ui.db, "env_vars").await, 0);

	// A read-only member sees the map without handles and without Connect.
	revoke_permission(&ui.db, role, "update", "application").await;
	revoke_permission(&ui.db, role, "update", "stack").await;
	let (_, html) = ui.get(&ui.url("")).await;
	let main = page_main(&html);
	assert_not_contains(main, "data-wire");
	assert_not_contains(main, "data-cv-connect");
	assert_not_contains(main, "Connect…");
	assert_contains(main, "data-targets=\"\"");
}

#[db_test]
async fn canvas_the_other_team_gets_404s(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let pg = ui.database("main").await;
	let (bob_token, bob_team) = user_with_team(&ui.app, "bob").await;
	let bob = ui_login(&ui.app, "bob").await;
	let (_, bob_project) = call(
		&ui.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob_token),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	let bob_project = bob_project["id"].as_i64().unwrap();
	let good = json!({ "cvFrom": format!("a-{web}"), "cvTo": format!("d-{pg}") });

	for path in [
		// Alice's team, by Bob: not a member.
		format!("/teams/{}/projects/{}/canvas", ui.team, ui.project),
		// Bob's team, Alice's project.
		format!("/teams/{bob_team}/projects/{}/canvas", ui.project),
		format!(
			"/teams/{bob_team}/projects/{}/canvas/wire/preview?datastar={}",
			ui.project,
			enc(&good.to_string())
		),
		// Bob's own project, with Alice's resource ids: they are not in it.
		format!(
			"/teams/{bob_team}/projects/{bob_project}/canvas/wire/preview?datastar={}",
			enc(&good.to_string())
		),
	] {
		let (status, _, _) = testing::get_html(&ui.app, &path, Some(&bob)).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
	}
	// Forged posts with Alice's ids, on Bob's team or in Bob's project, change nothing.
	for path in [
		format!("/teams/{bob_team}/projects/{}/canvas/wire", ui.project),
		format!("/teams/{bob_team}/projects/{bob_project}/canvas/wire"),
		format!("/teams/{}/projects/{}/canvas/wire", ui.team, ui.project),
	] {
		testing::post_signals(&ui.app, &path, Some(&bob), &good).await;
	}
	assert_eq!(count(&ui.db, "env_vars").await, 0);
}

#[db_test]
async fn canvas_the_status_poll_patches_only_the_nodes_that_moved(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let other = ui.application("api").await;
	seed_deployment(&ui.db, web, "building").await;
	seed_deployment(&ui.db, other, "running").await;
	let seen = format!("a-{web}=deploying;a-{other}=running");
	let (status, body) = ui.action_get("/status", &json!({ "seen": seen })).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, &format!("id=\"n-a-{web}\""));
	assert_not_contains(&body, &format!("id=\"n-a-{other}\""));
	assert_contains(&body, "data-flip");
	assert_contains(&body, "data-on-interval");
	// A first poll with nothing seen replays nothing.
	let (_, quiet) = ui.action_get("/status", &json!({ "seen": "" })).await;
	assert_not_contains(&quiet, "data-flip");
}

#[db_test]
async fn canvas_a_model_is_linked_from_the_canvas(db: PgPool) {
	let ui = Ui::new(db).await;
	let web = ui.application("web").await;
	let (_, model) = seed_model(&ui.db, ui.team, ui.project, "qwen", "running", None).await;
	let signals = json!({ "cvFrom": format!("m-{model}"), "cvTo": format!("a-{web}") });
	let (_, preview) = ui.action_get("/wire/preview", &signals).await;
	assert_contains(&preview, "Link web to qwen");
	assert_contains(&preview, "through the podkit gateway");
	assert_contains(&preview, "A new key scoped to qwen. Write-only.");
	let (_, done) = ui.post("/wire", &signals).await;
	assert_contains(&done, "web linked to qwen.");
	assert_eq!(count(&ui.db, "application_models").await, 1);
	let (_, html) = ui.get(&ui.url("")).await;
	assert_contains(&html, "data-kind=\"gateway\"");
	assert_contains(&html, "Model gateway link: web to qwen");
}

#[db_test]
async fn canvas_the_slug_canvas_is_reserved(db: PgPool) {
	let ui = Ui::new(db).await;
	let (status, v) = call(
		&ui.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/environments",
			ui.team, ui.project
		),
		Some(&ui.token),
		Some(json!({ "name": "Canvas", "slug": "canvas" })),
	)
	.await;
	assert!(status.is_client_error(), "{status} {v}");
}
