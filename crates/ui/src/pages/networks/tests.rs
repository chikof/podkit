//! Networks UI tests: the server's Networks tab, the resource panel, escaping, forged posts
//! and the other team's 404s. The fake agent answers the few agent calls a page makes.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{any, get};
use serde_json::{Value, json};

use crate::testing::{self, assert_contains, assert_not_contains, page_main, ui_login};
use crate::tests::{
	PgPool, call, count, db_test, member_role, revoke_permission, state, user_with_team,
};
use podkit_core::state::AppState;

async fn get_html(app: &Router, cookie: &str, uri: &str) -> (StatusCode, String) {
	let (status, _, body) = testing::get_html(app, uri, Some(cookie)).await;
	(status, body)
}

async fn post_signals(
	app: &Router,
	cookie: &str,
	uri: &str,
	signals: Value,
) -> (StatusCode, String) {
	let (status, _, body) = testing::post_signals(app, uri, Some(cookie), &signals).await;
	(status, body)
}

/// Answers `/v1/networks...` like an agent whose host has one extra network, `legacy`.
fn agent(state: &AppState) {
	let app = Router::new()
		.route(
			"/v1/networks",
			any(
				|method: axum::http::Method, body: axum::body::Bytes| async move {
					if method == axum::http::Method::GET {
						return axum::Json(json!([
							{ "name": "podkit", "internal": false, "driver": "bridge", "containers": [] },
							{ "name": "legacy", "internal": false, "driver": "bridge", "containers": [] },
						]))
						.into_response_compat();
					}
					let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
					(
						StatusCode::CREATED,
						axum::Json(json!({ "name": body["name"], "internal": false,
						"driver": "bridge", "containers": [] })),
					)
						.into_response_compat()
				},
			),
		)
		.route(
			"/v1/networks/{name}",
			get(
				|axum::extract::Path(name): axum::extract::Path<String>| async move {
					if name == "legacy" {
						axum::Json(json!({ "name": name, "internal": false, "driver": "bridge",
						"containers": [] }))
						.into_response_compat()
					} else {
						(
							StatusCode::NOT_FOUND,
							axum::Json(json!({ "error": "no such network" })),
						)
							.into_response_compat()
					}
				},
			)
			.delete(|| async { StatusCode::NO_CONTENT }),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
}

trait IntoResponseCompat {
	fn into_response_compat(self) -> axum::response::Response;
}

impl<T: axum::response::IntoResponse> IntoResponseCompat for T {
	fn into_response_compat(self) -> axum::response::Response {
		axum::response::IntoResponse::into_response(self)
	}
}

struct Ui {
	app: Router,
	cookie: String,
	token: String,
	db: PgPool,
	team: i64,
	server: i64,
	project: i64,
}

impl Ui {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		agent(&state);
		let app = crate::routes(state).await.unwrap();
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
			server: servers[0]["id"].as_i64().unwrap(),
			project: project["id"].as_i64().unwrap(),
		}
	}

	fn page(&self) -> String {
		format!("/teams/{}/servers/{}/networks", self.team, self.server)
	}

	async fn app_named(&self, name: &str) -> i64 {
		let (status, application) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications",
				self.team, self.project
			),
			Some(&self.token),
			Some(json!({
				"name": name, "slug": name, "server_id": self.server,
				"repo_url": "https://github.com/acme/x", "git_ref": "main",
				"container_port": 3000,
			})),
		)
		.await;
		assert!(status.is_success(), "{application}");
		application["id"].as_i64().unwrap()
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn networks_ui_tab_lists_networks_and_offers_the_actions(db: PgPool) {
	let ui = Ui::new(db).await;
	let (status, html) = get_html(&ui.app, &ui.cookie, &ui.page()).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&html);
	assert_contains(main, "No networks on this server yet.");
	assert_contains(main, "New network");
	assert_contains(main, "Import");
	assert_contains(main, "Migrate");
	assert_contains(main, "importable-networks");
	assert_contains(main, "value=\"legacy\"");
	assert_not_contains(main, "value=\"podkit\"");
	assert_eq!(html.matches("<h1").count(), 1, "one h1");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn networks_ui_create_import_and_delete_through_the_forms(db: PgPool) {
	let ui = Ui::new(db).await;
	let (status, _) = post_signals(
		&ui.app,
		&ui.cookie,
		&ui.page(),
		json!({ "net_name": "Backend <b>", "net_internal": true }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(count(&ui.db, "networks").await, 1);

	let (status, body) = post_signals(
		&ui.app,
		&ui.cookie,
		&format!("{}/import", ui.page()),
		json!({ "import_name": "missing" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "missing");
	assert_eq!(count(&ui.db, "networks").await, 1, "nothing was registered");

	let (_, body) = post_signals(
		&ui.app,
		&ui.cookie,
		&format!("{}/import", ui.page()),
		json!({ "import_name": "legacy" }),
	)
	.await;
	assert_not_contains(&body, "_error\":\"That");
	assert_eq!(count(&ui.db, "networks").await, 2);

	let (_, html) = get_html(&ui.app, &ui.cookie, &ui.page()).await;
	// User data is escaped, never markup.
	assert_contains(&html, "Backend &lt;b&gt;");
	assert_not_contains(&html, "Backend <b>");
	assert_contains(&html, "Imported");
	assert_contains(&html, "No outbound");

	let id: i64 =
		crate::tests::scalar(&ui.db, "SELECT id FROM networks WHERE imported", None).await;
	let (status, _) = post_signals(
		&ui.app,
		&ui.cookie,
		&format!("{}/{id}/delete", ui.page()),
		json!({}),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(count(&ui.db, "networks").await, 1);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn networks_ui_resource_tab_attaches_and_detaches(db: PgPool) {
	let ui = Ui::new(db).await;
	let app = ui.app_named("web").await;
	post_signals(
		&ui.app,
		&ui.cookie,
		&ui.page(),
		json!({ "net_name": "Shared", "net_internal": false }),
	)
	.await;
	let net: i64 =
		crate::tests::scalar(&ui.db, "SELECT id FROM networks WHERE kind = 'user'", None).await;
	let tab = format!(
		"/teams/{}/projects/{}/apps/{app}/networks",
		ui.team, ui.project
	);
	let (status, html) = get_html(&ui.app, &ui.cookie, &tab).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(html.matches("<h1").count(), 1);
	assert_contains(&html, "Networks");
	assert_contains(&html, "Shared");
	assert_contains(&html, "Attach network");

	let action = format!(
		"/teams/{}/projects/{}/networks/application/{app}",
		ui.team, ui.project
	);
	let (_, body) = post_signals(
		&ui.app,
		&ui.cookie,
		&format!("{action}/attach"),
		json!({ "net_id": net.to_string(), "net_aliases": "api, Back-End" }),
	)
	.await;
	assert_contains(&body, "lowercase letters");
	let (_, body) = post_signals(
		&ui.app,
		&ui.cookie,
		&format!("{action}/attach"),
		json!({ "net_id": net.to_string(), "net_aliases": "api, backend" }),
	)
	.await;
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	assert_contains(&body, "Redeploy now");
	assert_contains(&body, "backend");
	let (_, html) = get_html(&ui.app, &ui.cookie, &tab).await;
	assert_contains(&html, "Answers to");
	assert_not_contains(&html, "Attach a network");

	let (_, body) = post_signals(
		&ui.app,
		&ui.cookie,
		&format!("{action}/detach/{net}"),
		json!({}),
	)
	.await;
	assert_contains(&body, "Saved.");
	let on: i64 = crate::tests::scalar(
		&ui.db,
		"SELECT COUNT(*) FROM resource_networks WHERE network_id = $1",
		Some(net),
	)
	.await;
	assert_eq!(on, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn networks_ui_the_other_team_gets_404s_and_forged_posts_fail(db: PgPool) {
	let ui = Ui::new(db).await;
	let app = ui.app_named("web").await;
	post_signals(
		&ui.app,
		&ui.cookie,
		&ui.page(),
		json!({ "net_name": "Mine", "net_internal": false }),
	)
	.await;
	let net: i64 =
		crate::tests::scalar(&ui.db, "SELECT id FROM networks WHERE kind = 'user'", None).await;

	let (_, bob_team) = user_with_team(&ui.app, "bob").await;
	let bob = ui_login(&ui.app, "bob").await;
	for path in [
		format!("/teams/{bob_team}/servers/{}/networks", ui.server),
		format!(
			"/teams/{bob_team}/projects/{}/apps/{app}/networks",
			ui.project
		),
	] {
		let (status, _) = get_html(&ui.app, &bob, &path).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
	}
	// Forged actions with Alice's ids on Bob's team change nothing.
	for (path, signals) in [
		(
			format!(
				"/teams/{bob_team}/projects/{}/networks/application/{app}/attach",
				ui.project
			),
			json!({ "net_id": net.to_string() }),
		),
		(
			format!(
				"/teams/{bob_team}/servers/{}/networks/{net}/delete",
				ui.server
			),
			json!({}),
		),
	] {
		post_signals(&ui.app, &bob, &path, signals).await;
	}
	assert_eq!(count(&ui.db, "networks").await, 1);
	assert_eq!(count(&ui.db, "resource_networks").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn networks_ui_a_member_without_update_sees_no_actions_and_forged_posts_fail(db: PgPool) {
	let ui = Ui::new(db).await;
	let app = ui.app_named("web").await;
	let role = member_role(&ui.db, ui.team, "alice").await;
	revoke_permission(&ui.db, role, "update", "server").await;
	revoke_permission(&ui.db, role, "update", "application").await;

	let (status, html) = get_html(&ui.app, &ui.cookie, &ui.page()).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&html);
	assert_not_contains(main, "New network");
	assert_not_contains(main, "Migrate");

	let (_, body) = post_signals(
		&ui.app,
		&ui.cookie,
		&ui.page(),
		json!({ "net_name": "Nope", "net_internal": false }),
	)
	.await;
	assert_contains(&body, "permission");
	assert_eq!(count(&ui.db, "networks").await, 0);

	let (_, html) = get_html(
		&ui.app,
		&ui.cookie,
		&format!(
			"/teams/{}/projects/{}/apps/{app}/networks",
			ui.team, ui.project
		),
	)
	.await;
	assert_not_contains(&html, "Attach network");
}
