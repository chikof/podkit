//! Tests of the projects grid, the environment view, the environment actions and the
//! create-resource picker.
//!
//! The tests of the old project pages live on here under their old names or an extended
//! one: the list, create, delete, escaping, tenancy and permission tests.

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

use crate::tests::grid::{
	add_redis_databases, delete_databases, env_id, environment_name, finish_deployments,
	move_databases_to, set_application_nixpacks, set_database_name, set_database_status,
};
use crate::tests::{
	PgPool, app, call, count, db_test, project_id, rename_applications, seed_app_and_database,
	seed_deployment, seed_model, user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, send, ui_login,
	ui_session,
};

async fn project_count(db: &PgPool) -> i64 {
	count(db, "projects").await
}

/// Percent-encodes a Datastar signal object for the `?datastar=` query of a GET.
fn signals_query(signals: &serde_json::Value) -> String {
	use std::fmt::Write as _;
	let mut out = String::new();
	for b in signals.to_string().bytes() {
		if b.is_ascii_alphanumeric() {
			out.push(char::from(b));
		} else {
			let _ = write!(out, "%{b:02X}");
		}
	}
	format!("datastar={out}")
}

async fn make_project(app: &Router, cookie: &str, team: i64, name: &str, slug: &str) {
	let (s, _, body) = post_signals(
		app,
		&format!("/teams/{team}/projects"),
		Some(cookie),
		&json!({ "name": name, "slug": slug }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "window.location.assign");
}

/// Makes an environment through the UI action and returns its id.
async fn make_environment(
	app: &Router,
	db: &PgPool,
	cookie: &str,
	team: i64,
	project: i64,
	name: &str,
	slug: &str,
) -> i64 {
	let (s, _, body) = post_signals(
		app,
		&format!("/teams/{team}/projects/{project}/environments"),
		Some(cookie),
		&json!({ "newName": name, "newSlug": slug }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, &format!("/projects/{project}/{slug}"));
	env_id(db, project, slug).await
}

/// Adds `name` to `team` with the built-in read-only Member role (id 2).
async fn join_as_member(app: &Router, owner_token: &str, team: i64, name: &str) {
	let (s, _) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(owner_token),
		Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
	)
	.await;
	assert!(s.is_success(), "add member: {s}");
}

fn flash_set(headers: &axum::http::HeaderMap) -> bool {
	headers
		.get_all("set-cookie")
		.iter()
		.any(|c| c.to_str().unwrap().starts_with("flash="))
}

// --- the grid (was `project::tests`) ---------------------------------------------------

/// Was `the_list_shows_projects_with_counts_and_the_forms`.
#[db_test]
async fn the_grid_shows_projects_with_counts_logos_and_the_forms(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let list = format!("/teams/{team}/projects");

	let (s, _, body) = get_html(&app, &list, Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "No projects yet");
	assert_contains(&body, "New project");
	assert_contains(&body, "<title>Projects");
	assert_contains(&body, "S3 Destinations");

	let (s, headers, body) = post_signals(
		&app,
		&list,
		Some(&cookie),
		&json!({ "name": "Web shop", "slug": "web-shop" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let id = project_id(&db, "web-shop").await;
	assert_contains(
		&body,
		&format!("window.location.assign('/teams/{team}/projects/{id}')"),
	);
	assert!(flash_set(&headers), "a toast waits for the next page");
	assert_eq!(project_count(&db).await, 1);

	let (_, _, body) = get_html(&app, &list, Some(&cookie)).await;
	assert_contains(&body, "Web shop");
	assert_contains(&body, "web-shop");
	assert_contains(&body, "Nothing deployed yet");
	assert_contains(&body, "1 environment");
	assert_contains(&body, "Never deployed");
	assert_contains(&body, "No resources yet");
	assert_contains(
		&body,
		&format!("href=\"/teams/{team}/projects/{id}/production\""),
	);

	seed_app_and_database(&db, team, id).await;
	let (_, _, body) = get_html(&app, &list, Some(&cookie)).await;
	assert_contains(&body, "1 application · 1 database");
	assert_contains(&body, "data-logo=\"postgresql\"");
	assert_contains(&body, "Contains Storefront, orders");
}

#[db_test]
async fn the_grid_shows_five_logos_and_the_rest_as_a_count(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Big", "big").await;
	let project = project_id(&db, "big").await;
	seed_app_and_database(&db, team, project).await;
	add_redis_databases(&db, project, &["a", "b", "c", "d", "e"]).await;
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	// One application and six databases: seven logos, five drawn.
	assert_eq!(page_main(&body).matches("data-logo=").count(), 5);
	assert_contains(&body, ">+2<");
}

/// Was `creating_rejects_bad_and_duplicate_input_with_a_message`.
#[db_test]
async fn creating_rejects_bad_and_duplicate_input_with_a_message(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let list = format!("/teams/{team}/projects");

	let (_, _, body) = post_signals(
		&app,
		&list,
		Some(&cookie),
		&json!({ "name": "", "slug": "x" }),
	)
	.await;
	assert_contains(&body, "Name must be");
	let (_, _, body) = post_signals(
		&app,
		&list,
		Some(&cookie),
		&json!({ "name": "X", "slug": "Not A Slug" }),
	)
	.await;
	assert_contains(&body, "Slug must be");

	let ok = json!({ "name": "One", "slug": "one" });
	post_signals(&app, &list, Some(&cookie), &ok).await;
	let (s, _, body) = post_signals(&app, &list, Some(&cookie), &ok).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "A project with that slug already exists.");
	assert_eq!(project_count(&db).await, 1);
}

/// Was `deleting_needs_the_slug_typed_exactly`.
#[db_test]
async fn deleting_needs_the_slug_typed_exactly(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let list = format!("/teams/{team}/projects");
	make_project(&app, &cookie, team, "Doomed", "doomed").await;
	let id = project_id(&db, "doomed").await.to_string();
	let delete = format!("{list}/delete");

	let (_, _, body) = post_signals(
		&app,
		&delete,
		Some(&cookie),
		&json!({ "target": id, "confirm": "nope" }),
	)
	.await;
	assert_contains(&body, "doesn't match");
	assert_eq!(project_count(&db).await, 1);

	let (s, _, body) = post_signals(
		&app,
		&delete,
		Some(&cookie),
		&json!({ "target": id, "confirm": "doomed" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, &format!("window.location.assign('{list}')"));
	assert_eq!(project_count(&db).await, 0);

	let (_, _, body) = post_signals(
		&app,
		&delete,
		Some(&cookie),
		&json!({ "target": "", "confirm": "" }),
	)
	.await;
	assert_contains(&body, "Pick a project");
}

#[db_test]
async fn renaming_changes_the_name_and_keeps_the_slug_and_the_link(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Old name", "keep-me").await;
	let id = project_id(&db, "keep-me").await;
	let rename = format!("/teams/{team}/projects/rename");

	let (_, _, body) = post_signals(
		&app,
		&rename,
		Some(&cookie),
		&json!({ "target": id.to_string(), "name": "  " }),
	)
	.await;
	assert_contains(&body, "Name must be");

	let (s, _, body) = post_signals(
		&app,
		&rename,
		Some(&cookie),
		&json!({ "target": id.to_string(), "name": "New name" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "window.location.assign");
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	assert_contains(&body, "New name");
	assert_not_contains(page_main(&body), "Old name");
	assert_contains(&body, "keep-me");

	let (_, _, body) = post_signals(
		&app,
		&rename,
		Some(&cookie),
		&json!({ "target": "", "name": "x" }),
	)
	.await;
	assert_contains(&body, "Pick a project");
}

/// Was `project_names_are_escaped`, now with environment and resource names too.
#[db_test]
async fn project_environment_and_resource_names_are_escaped(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let list = format!("/teams/{team}/projects");
	let name = "<script>alert(1)</script>";
	let escaped = "&lt;script&gt;alert(1)&lt;/script&gt;";

	make_project(&app, &cookie, team, name, "xss").await;
	let project = project_id(&db, "xss").await;
	seed_app_and_database(&db, team, project).await;
	rename_applications(&db, name).await;
	set_database_name(&db, name).await;
	make_environment(&app, &db, &cookie, team, project, name, "xss-env").await;

	for uri in [
		list.clone(),
		format!("{list}/{project}"),
		format!("{list}/{project}/production"),
		format!("{list}/{project}/xss-env"),
		format!("{list}/{project}/production?q=script&type=all"),
	] {
		let (s, _, body) = get_html(&app, &uri, Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_not_contains(&body, name);
		assert_contains(&body, escaped);
	}

	// The search text itself, and the fragment that answers the filter, are escaped too.
	let (_, _, body) = get_html(
		&app,
		&format!("{list}/{project}/production?q=%3Cb%3Ex"),
		Some(&cookie),
	)
	.await;
	assert_not_contains(&body, "<b>x");
	assert_contains(&body, "&lt;b&gt;x");
	let (s, _, body) = get_html(
		&app,
		&format!(
			"{list}/{project}/production/resources?{}",
			signals_query(&json!({ "q": "", "type": "all" }))
		),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, name);
	assert_contains(&body, escaped);
}

/// Was `another_teams_pages_are_the_404_page_and_leak_nothing`, plus environments.
#[db_test]
async fn another_teams_pages_are_the_404_page_and_leak_nothing(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, alice_team) = ui_session(&app, "alice").await;
	let (bob, bob_team) = ui_session(&app, "bob").await;
	make_project(&app, &alice, alice_team, "Secret plans", "secret-plans").await;
	let secret = project_id(&db, "secret-plans").await;
	let prod = env_id(&db, secret, "production").await;
	let staging = make_environment(
		&app,
		&db,
		&alice,
		alice_team,
		secret,
		"Hidden stage",
		"hidden-stage",
	)
	.await;
	seed_app_and_database(&db, alice_team, secret).await;

	// Bob's own project, to try Alice's environment against.
	make_project(&app, &bob, bob_team, "Bobs", "bobs").await;
	let bobs = project_id(&db, "bobs").await;

	for uri in [
		format!("/teams/{alice_team}/projects"),
		format!("/teams/{alice_team}/projects/{secret}"),
		format!("/teams/{alice_team}/projects/{secret}/production"),
		format!("/teams/{alice_team}/projects/{secret}/hidden-stage"),
		format!("/teams/{alice_team}/projects/{secret}/production/resources"),
		format!("/teams/{alice_team}/projects/{secret}?environment={prod}"),
		// Bob's own team, Alice's project id.
		format!("/teams/{bob_team}/projects/{secret}"),
		format!("/teams/{bob_team}/projects/{secret}/production"),
		format!("/teams/{bob_team}/projects/{secret}/production/resources"),
		// Bob's own project, Alice's environment (by id and by slug).
		format!("/teams/{bob_team}/projects/{bobs}?environment={staging}"),
		format!("/teams/{bob_team}/projects/{bobs}/hidden-stage"),
		format!("/teams/{bob_team}/projects/{bobs}/hidden-stage/resources"),
		"/teams/999/projects".to_owned(),
		"/teams/nope/projects".to_owned(),
	] {
		let (s, _, body) = get_html(&app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_contains(&body, "Not found");
		for leaked in [
			"Secret plans",
			"secret-plans",
			"Hidden stage",
			"Storefront",
			"orders",
			"alice",
		] {
			assert_not_contains(&body, leaked);
		}
	}

	// Actions through Bob's team on Alice's project or environment change nothing.
	let gone = "That no longer exists.";
	let (_, _, body) = post_signals(
		&app,
		&format!("/teams/{alice_team}/projects"),
		Some(&bob),
		&json!({ "name": "Planted", "slug": "planted" }),
	)
	.await;
	assert_contains(&body, gone);
	let (_, _, body) = post_signals(
		&app,
		&format!("/teams/{bob_team}/projects/delete"),
		Some(&bob),
		&json!({ "target": secret.to_string(), "confirm": "secret-plans" }),
	)
	.await;
	assert_contains(&body, gone);
	let (_, _, body) = post_signals(
		&app,
		&format!("/teams/{bob_team}/projects/rename"),
		Some(&bob),
		&json!({ "target": secret.to_string(), "name": "Mine now" }),
	)
	.await;
	assert_contains(&body, gone);
	let (_, _, body) = post_signals(
		&app,
		&format!("/teams/{bob_team}/projects/{secret}/environments"),
		Some(&bob),
		&json!({ "newName": "X", "newSlug": "x" }),
	)
	.await;
	assert_contains(&body, gone);
	for (path, signals) in [
		(
			"environments/rename",
			json!({ "envId": staging.to_string(), "envName": "Mine" }),
		),
		(
			"environments/delete",
			json!({ "envId": staging.to_string() }),
		),
	] {
		for project in [secret, bobs] {
			let (_, _, body) = post_signals(
				&app,
				&format!("/teams/{bob_team}/projects/{project}/{path}"),
				Some(&bob),
				&signals,
			)
			.await;
			assert_contains(&body, gone);
		}
	}
	assert_eq!(project_count(&db).await, 2);
	assert_eq!(count(&db, "environments").await, 3);
	assert_eq!(environment_name(&db, staging).await, "Hidden stage");
}

/// Was `a_member_without_create_and_delete_sees_no_actions_and_forged_posts_fail`.
#[db_test]
async fn a_member_without_create_and_delete_sees_no_actions_and_forged_posts_fail(db: PgPool) {
	let app = app(db.clone()).await;
	let (owner_token, team) = user_with_team(&app, "alice").await;
	let owner = ui_login(&app, "alice").await;
	let (_, _) = user_with_team(&app, "carol").await;
	join_as_member(&app, &owner_token, team, "carol").await;
	let carol = ui_login(&app, "carol").await;
	let list = format!("/teams/{team}/projects");
	make_project(&app, &owner, team, "Shared", "shared").await;
	let project = project_id(&db, "shared").await;
	let prod = env_id(&db, project, "production").await;
	seed_app_and_database(&db, team, project).await;

	// The grid and the environment show the resources but none of the actions.
	for uri in [list.clone(), format!("{list}/{project}/production")] {
		let (s, _, body) = get_html(&app, &uri, Some(&carol)).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		let main = page_main(&body);
		assert_contains(main, "Shared");
		for action in [
			"New project",
			"Create project",
			"Delete",
			"Rename",
			"New environment",
			"Create a resource",
			"Create environment",
		] {
			assert_not_contains(main, action);
		}
	}
	let (_, _, body) = get_html(&app, &format!("{list}/{project}"), Some(&carol)).await;
	assert_contains(page_main(&body), "Storefront");

	// Forged posts are refused by the service.
	let denied = "You don't have permission to do that.";
	for (path, signals) in [
		(list.clone(), json!({ "name": "Forged", "slug": "forged" })),
		(
			format!("{list}/delete"),
			json!({ "target": project.to_string(), "confirm": "shared" }),
		),
		(
			format!("{list}/rename"),
			json!({ "target": project.to_string(), "name": "Forged" }),
		),
		(
			format!("{list}/{project}/environments"),
			json!({ "newName": "Forged", "newSlug": "forged" }),
		),
		(
			format!("{list}/{project}/environments/rename"),
			json!({ "envId": prod.to_string(), "envName": "Forged" }),
		),
		(
			format!("{list}/{project}/environments/delete"),
			json!({ "envId": prod.to_string() }),
		),
	] {
		let (_, _, body) = post_signals(&app, &path, Some(&carol), &signals).await;
		assert_contains(&body, denied);
	}
	assert_eq!(project_count(&db).await, 1);
	assert_eq!(count(&db, "environments").await, 1);
	let (_, _, body) = get_html(&app, &format!("{list}/{project}"), Some(&carol)).await;
	assert_contains(page_main(&body), "Shared");

	// The owner does get the actions.
	let (_, _, body) = get_html(&app, &list, Some(&owner)).await;
	assert_contains(&body, "New project");
	assert_contains(&body, "Delete");
	assert_contains(&body, "Rename");
	let (_, _, body) = get_html(&app, &format!("{list}/{project}"), Some(&owner)).await;
	assert_contains(&body, "Create a resource");
	assert_contains(&body, "New environment");
}

#[db_test]
async fn an_unauthenticated_action_is_sent_to_sign_in(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	for (path, signals) in [
		(
			format!("/teams/{team}/projects"),
			json!({ "name": "Anon", "slug": "anon" }),
		),
		(
			format!("/teams/{team}/projects/rename"),
			json!({ "target": "1", "name": "Anon" }),
		),
		(
			format!("/teams/{team}/projects/1/environments"),
			json!({ "newName": "Anon", "newSlug": "anon" }),
		),
	] {
		let (s, _, body) = post_signals(&app, &path, None, &signals).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "window.location.assign('/login')");
	}
	assert_eq!(project_count(&db).await, 0);
}

#[db_test]
async fn cross_origin_actions_are_refused(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (s, _, _) = send(
		&app,
		"POST",
		&format!("/teams/{team}/projects"),
		Some(&cookie),
		Some(&json!({ "name": "Evil", "slug": "evil" }).to_string()),
		Some("http://evil.example"),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(project_count(&db).await, 0);
}

// --- the environment view ----------------------------------------------------------------

/// Was `the_overview_lists_applications_and_databases`.
#[db_test]
async fn the_environment_lists_resources_with_logos_status_and_links(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	seed_app_and_database(&db, team, project).await;
	seed_model(&db, team, project, "Llama", "running", Some("c1")).await;

	for uri in [
		format!("/teams/{team}/projects/{project}"),
		format!("/teams/{team}/projects/{project}/production"),
	] {
		let (s, _, body) = get_html(&app, &uri, Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		let main = page_main(&body);
		assert_contains(main, "Storefront");
		assert_contains(main, "example.com/r");
		assert_contains(main, "orders");
		assert_contains(main, "PostgreSQL 17");
		assert_contains(main, "/databases/6001");
		assert_contains(main, "/apps/5001");
		assert_contains(main, "Llama");
		assert_contains(main, "Pending");
		assert_contains(main, "Not deployed");
		assert_contains(main, "Running");
		// A type badge on each, a logo on each (a mark or the monogram).
		for kind in ["Application", "Database", "Model"] {
			assert_contains(main, &format!(">{kind}<"));
		}
		assert_eq!(main.matches("data-resource=").count(), 3, "{main}");
		assert!(main.matches("data-logo=").count() >= 3, "{main}");
		assert_contains(main, "data-logo=\"postgresql\"");
		assert_contains(main, "data-logo=\"huggingface\"");
		assert_contains(main, ".sslip.io");
		assert_contains(main, "aria-current=\"page\"");
	}
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	assert_contains(&body, "1 application · 1 database · 1 model");
}

#[db_test]
async fn a_resource_without_a_known_mark_gets_a_monogram(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	seed_app_and_database(&db, team, project).await;
	set_application_nixpacks(&db, "Zeta").await;
	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{team}/projects/{project}"),
		Some(&cookie),
	)
	.await;
	assert_contains(page_main(&body), "data-logo=\"monogram\"");
	assert_contains(page_main(&body), ">Z<");
}

#[db_test]
async fn environments_are_separate_tabs_with_their_own_resources(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	seed_app_and_database(&db, team, project).await;
	let staging = make_environment(&app, &db, &cookie, team, project, "Staging", "staging").await;
	move_databases_to(&db, staging).await;

	let base = format!("/teams/{team}/projects/{project}");
	let (_, _, prod) = get_html(&app, &format!("{base}/production"), Some(&cookie)).await;
	let (_, _, stage) = get_html(&app, &format!("{base}/staging"), Some(&cookie)).await;
	assert_contains(page_main(&prod), "Storefront");
	assert_not_contains(page_main(&prod), "PostgreSQL 17");
	assert_contains(page_main(&stage), "PostgreSQL 17");
	assert_not_contains(page_main(&stage), "Storefront");
	// Both tabs are on both pages, and only the one shown is current.
	for body in [&prod, &stage] {
		assert_contains(body, &format!("href=\"{base}/production\""));
		assert_contains(body, &format!("href=\"{base}/staging\""));
		assert_eq!(page_main(body).matches("aria-current=\"page\"").count(), 1);
	}
	// The omitted environment is the first; `?environment=` still picks one by id.
	let (_, _, bare) = get_html(&app, &base, Some(&cookie)).await;
	assert_contains(page_main(&bare), "Storefront");
	let (_, _, by_id) = get_html(
		&app,
		&format!("{base}?environment={staging}"),
		Some(&cookie),
	)
	.await;
	assert_contains(page_main(&by_id), "PostgreSQL 17");
	let (s, _, _) = get_html(&app, &format!("{base}/nope"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, _, _) = get_html(&app, &format!("{base}?environment=1"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test]
async fn an_empty_environment_says_so_and_offers_to_create(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{team}/projects/{project}"),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, "Nothing in production yet");
	assert_contains(page_main(&body), "Create");
}

#[db_test]
async fn the_filter_narrows_by_type_and_search_on_the_page_and_the_fragment(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	seed_app_and_database(&db, team, project).await;
	let base = format!("/teams/{team}/projects/{project}/production");

	let (_, _, body) = get_html(&app, &format!("{base}?type=database"), Some(&cookie)).await;
	assert_contains(page_main(&body), "orders");
	assert_not_contains(page_main(&body), "Storefront");
	assert_contains(page_main(&body), "<option value=\"database\" selected");
	let (_, _, body) = get_html(&app, &format!("{base}?q=STORE"), Some(&cookie)).await;
	assert_contains(page_main(&body), "Storefront");
	assert_not_contains(page_main(&body), "orders");
	assert_contains(page_main(&body), "value=\"STORE\"");

	let fragment =
		|signals: &serde_json::Value| format!("{base}/resources?{}", signals_query(signals));
	let (s, _, body) = get_html(
		&app,
		&fragment(&json!({ "q": "", "type": "application" })),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "id=\"resource-grid\"");
	assert_contains(&body, "Storefront");
	assert_not_contains(&body, "orders");
	assert_not_contains(&body, "<main");
	let (_, _, body) = get_html(
		&app,
		&fragment(&json!({ "q": "zzz", "type": "all" })),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, "Nothing matches");
	assert_contains(&body, "Clear the filter");
	let (_, _, body) = get_html(
		&app,
		&fragment(&json!({ "q": "", "type": "all" })),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, "2 resources");
	// An unknown type is no filter, and no signals at all is the whole grid.
	let (_, _, body) = get_html(
		&app,
		&fragment(&json!({ "q": "", "type": "bogus" })),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, "2 resources");
	let (s, _, body) = get_html(&app, &format!("{base}/resources"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "2 resources");
}

#[db_test]
async fn the_grid_polls_only_while_something_is_changing(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	seed_app_and_database(&db, team, project).await;
	let uri = format!("/teams/{team}/projects/{project}/production");

	// The seeded database is pending: still changing.
	let (_, _, body) = get_html(&app, &uri, Some(&cookie)).await;
	assert_contains(&body, "data-on-interval__duration.3s");
	assert_contains(&body, &format!("{uri}/resources"));

	set_database_status(&db, "running").await;
	seed_deployment(&db, 5001, "building").await;
	let (_, _, body) = get_html(&app, &uri, Some(&cookie)).await;
	assert_contains(&body, "Building");
	assert_contains(&body, "data-on-interval__duration.3s");

	finish_deployments(&db).await;
	let (_, _, body) = get_html(&app, &uri, Some(&cookie)).await;
	assert_not_contains(&body, "data-on-interval__duration.3s");
	assert_contains(page_main(&body), "Deployed ");
}

// --- environment actions -------------------------------------------------------------------

#[db_test]
async fn environments_can_be_created_renamed_and_deleted_from_the_ui(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	let base = format!("/teams/{team}/projects/{project}");
	let envs = format!("{base}/environments");
	let prod = env_id(&db, project, "production").await;
	let act = |path: String, signals: serde_json::Value| {
		let app = app.clone();
		let cookie = cookie.clone();
		async move { post_signals(&app, &path, Some(&cookie), &signals).await }
	};

	// Create: bad input and a taken slug are said inline, a reserved slug is refused.
	let (_, _, body) = act(envs.clone(), json!({ "newName": "", "newSlug": "x" })).await;
	assert_contains(&body, "Name must be");
	let (_, _, body) = act(
		envs.clone(),
		json!({ "newName": "X", "newSlug": "Bad Slug" }),
	)
	.await;
	assert_contains(&body, "Slug must be");
	let (_, _, body) = act(
		envs.clone(),
		json!({ "newName": "Prod 2", "newSlug": "production" }),
	)
	.await;
	assert_contains(&body, "already exists");
	for reserved in ["apps", "databases", "models", "environments"] {
		let (_, _, body) = act(envs.clone(), json!({ "newName": "R", "newSlug": reserved })).await;
		assert_contains(&body, "is reserved");
	}
	assert_eq!(count(&db, "environments").await, 1);

	let (s, headers, body) = act(
		envs.clone(),
		json!({ "newName": "Staging", "newSlug": "staging" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, &format!("window.location.assign('{base}/staging')"));
	assert!(flash_set(&headers));
	let staging = env_id(&db, project, "staging").await;

	// Rename keeps the slug, so the link keeps working.
	let rename = format!("{envs}/rename");
	let (_, _, body) = act(
		rename.clone(),
		json!({ "envId": staging.to_string(), "envName": "" }),
	)
	.await;
	assert_contains(&body, "Name must be");
	let (_, _, body) = act(
		rename.clone(),
		json!({ "envId": staging.to_string(), "envName": "Pre-prod" }),
	)
	.await;
	assert_contains(&body, &format!("window.location.assign('{base}/staging')"));
	let (_, _, page) = get_html(&app, &format!("{base}/staging"), Some(&cookie)).await;
	assert_contains(page_main(&page), "Pre-prod");
	let (_, _, body) = act(rename, json!({ "envId": "nope", "envName": "x" })).await;
	assert_contains(&body, "Pick an environment");

	// Delete: refused while it holds resources (the message names them, inline), and for
	// the last environment; fine once empty.
	seed_app_and_database(&db, team, project).await;
	move_databases_to(&db, staging).await;
	let delete = format!("{envs}/delete");
	let (s, _, body) = act(delete.clone(), json!({ "envId": prod.to_string() })).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "still has resources");
	assert_contains(&body, "application Storefront");
	assert_not_contains(&body, "window.location.assign");
	assert_eq!(count(&db, "environments").await, 2);
	let (_, _, body) = act(delete.clone(), json!({ "envId": staging.to_string() })).await;
	assert_contains(&body, "still has resources");
	assert_contains(&body, "database orders");

	delete_databases(&db).await;
	let (_, _, body) = act(delete.clone(), json!({ "envId": staging.to_string() })).await;
	assert_contains(&body, &format!("window.location.assign('{base}')"));
	assert_eq!(count(&db, "environments").await, 1);
	let (_, _, body) = act(delete, json!({ "envId": prod.to_string() })).await;
	assert_contains(&body, "at least one environment");
	assert_eq!(count(&db, "environments").await, 1);
}

// --- the picker -------------------------------------------------------------------------

#[db_test]
async fn the_picker_routes_each_kind_to_its_create_flow(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	make_project(&app, &cookie, team, "Shop", "shop").await;
	let project = project_id(&db, "shop").await;
	let prod = env_id(&db, project, "production").await;
	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{team}/projects/{project}"),
		Some(&cookie),
	)
	.await;
	let main = page_main(&body);

	assert_contains(main, "id=\"create-resource\"");
	assert_contains(
		main,
		"command=\"show-modal\" commandfor=\"create-resource\"",
	);
	assert_contains(main, "closedby=\"any\"");
	let base = format!("/teams/{team}/projects/{project}");
	assert_contains(
		main,
		&format!("href=\"{base}/apps/new?environment={prod}\""),
	);
	for engine in ["postgres", "mysql", "mongodb", "redis"] {
		assert_contains(
			main,
			&format!("{base}/databases/new?environment={prod}&amp;engine={engine}"),
		);
	}
	assert_contains(main, "data-logo=\"mongodb\"");
	assert_contains(main, "/models/catalog?project=");
	// Compose and templates are links into the project, carrying the environment.
	assert_contains(
		main,
		&format!("href=\"{base}/stacks/new?environment={prod}\""),
	);
	assert_contains(
		main,
		&format!("href=\"{base}/templates?environment={prod}\""),
	);
	assert_not_contains(main, "Soon");
	// Docker images stay hidden until their create page exists.
	assert_not_contains(main, "Docker image");
}
