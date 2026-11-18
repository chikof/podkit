//! Tests of the frame every team page sits in: the sidebar, the header's switchers, the
//! command palette's search, the settings pages and the flash message.
//!
//! Old tests of pages that now render inside the shell are kept where they were (each
//! page's own module); these cover what the shell itself adds.

use axum::Router;
use axum::http::{StatusCode, header};
use serde_json::json;

use crate::tests::{PgPool, app, call, db_test, team_name, user_id, user_with_team};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, send, ui_login,
	ui_session,
};

const MEMBER: i64 = 2;

async fn project(app: &Router, token: &str, team: i64, slug: &str, name: &str) -> i64 {
	let (s, body) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(token),
		Some(json!({ "name": name, "slug": slug })),
	)
	.await;
	assert!(s.is_success(), "create project {slug}: {s} {body}");
	body["id"].as_i64().unwrap()
}

async fn register(app: &Router, name: &str) {
	let creds = json!({
		"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
	});
	let (s, _) = call(app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(s.is_success());
}

/// alice owns a team; bob joins it as a plain member. (alice cookie, bob cookie, team)
async fn team_with_member(app: &Router) -> (String, String, i64) {
	let (alice, team) = ui_session(app, "alice").await;
	register(app, "bob").await;
	let (s, _, _) = post_signals(
		app,
		&format!("/teams/{team}/members"),
		Some(&alice),
		&json!({ "email": "bob@example.com", "newrole": MEMBER.to_string() }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	(alice, ui_login(app, "bob").await, team)
}

/// The text of the SSE patch a search answers with.
async fn search(app: &Router, cookie: &str, team: i64, q: &str) -> (StatusCode, String) {
	let (s, _, body) = send(
		app,
		"GET",
		&format!("/teams/{team}/search?q={q}"),
		Some(cookie),
		None,
		None,
	)
	.await;
	(s, body)
}

fn flash_cookie(h: &axum::http::HeaderMap) -> Option<String> {
	h.get_all(header::SET_COOKIE)
		.iter()
		.filter_map(|v| v.to_str().ok())
		.find(|c| c.starts_with("flash=") && !c.starts_with("flash=;"))
		.map(str::to_owned)
}

// --- the frame ------------------------------------------------------------------------

#[db_test]
async fn every_team_page_renders_inside_the_shell(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	for path in [
		format!("/teams/{team}/projects"),
		format!("/teams/{team}/servers"),
		format!("/teams/{team}/models"),
		format!("/teams/{team}/models/catalog"),
		format!("/teams/{team}/models/settings"),
		format!("/teams/{team}/backup-targets"),
		format!("/teams/{team}/members"),
		format!("/teams/{team}/settings"),
		format!("/teams/{team}/settings/registration"),
		format!("/teams/{team}/settings/appearance"),
	] {
		let (s, _, body) = get_html(&app, &path, Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK, "{path}");
		for marker in [
			"class=\"ui-shell\"",
			"class=\"ui-sidebar ui-vt-chrome\"",
			"id=\"nav-drawer\"",
			"id=\"palette\"",
			"<main id=\"main\"",
			"<link rel=\"expect\" href=\"#main\" blocking=\"render\">",
			"aria-label=\"Breadcrumb\"",
		] {
			assert!(body.contains(marker), "{path}: missing {marker}");
		}
	}
	// A page without a team (sign-in) stays bare.
	let (_, _, login) = get_html(&app, "/login", None).await;
	assert_not_contains(&login, "ui-sidebar");
	assert_not_contains(&login, "id=\"palette\"");
}

#[db_test]
async fn the_sidebar_lists_the_places_and_marks_the_current_one(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;

	let (_, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
	for label in [
		"Projects",
		"Templates",
		"Servers",
		"Models",
		"S3 Destinations",
		"Members",
		"Registration",
		"Hugging Face token",
		"Appearance",
	] {
		assert_contains(
			&body,
			&format!("<span class=\"ui-nav-label\">{label}</span>"),
		);
	}
	// The sidebar's and the drawer's copy of the one current link, and no other.
	let current = format!(
		"href=\"/teams/{team}/servers\" class=\"ui-nav-link\" title=\"Servers\" aria-current=\"page\""
	);
	assert_eq!(body.matches(&current).count(), 2, "{body}");
	assert_eq!(body.matches("class=\"ui-nav-link\"").count() % 2, 0);
	// The user menu has the theme choice and sign out.
	assert_contains(&body, "action=\"/logout\"");
	assert_contains(&body, "name=\"theme\" value=\"dark\"");
}

#[db_test]
async fn a_link_the_role_may_not_use_is_not_rendered(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = ui_session(&app, "alice").await;
	register(&app, "carol").await;
	let carol = user_id(&db, "carol").await;
	// A role that reads projects and nothing else.
	crate::tests::join_as_project_reader(&db, team, carol, 9001).await;
	let cookie = ui_login(&app, "carol").await;

	let (s, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "<span class=\"ui-nav-label\">Projects</span>");
	for hidden in [
		"Servers",
		"Models",
		"S3 Destinations",
		"Members",
		"Hugging Face token",
	] {
		assert_not_contains(
			&body,
			&format!("<span class=\"ui-nav-label\">{hidden}</span>"),
		);
	}
	// The palette offers no action the role may not take either.
	for hidden in ["Create project", "Add server", "Add S3 destination"] {
		assert_not_contains(&body, hidden);
	}
}

#[db_test]
async fn a_folded_sidebar_is_rendered_folded_from_the_cookie(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let page = format!("/teams/{team}/projects");

	let (_, _, open) = get_html(&app, &page, Some(&cookie)).await;
	assert_contains(&open, "data-signals=\"{_collapsed: false}\"");
	assert_not_contains(&open, "class=\"ui-shell\" data-collapsed");

	let folded = format!("{cookie}; sidebar=collapsed");
	let (_, _, body) = get_html(&app, &page, Some(&folded)).await;
	assert_contains(&body, "class=\"ui-shell\" data-collapsed");
	assert_contains(&body, "data-signals=\"{_collapsed: true}\"");
	assert_contains(&body, "aria-pressed=\"true\"");
}

// --- breadcrumbs and switchers --------------------------------------------------------

#[db_test]
async fn the_header_switches_team_project_and_environment(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let cookie = ui_login(&app, "alice").await;
	let api = project(&app, &token, team, "api", "api").await;
	project(&app, &token, team, "web", "web").await;
	let (s, staging) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects/{api}/environments"),
		Some(&token),
		Some(json!({ "name": "staging", "slug": "staging" })),
	)
	.await;
	assert!(s.is_success(), "{staging}");
	let staging = staging["id"].as_i64().unwrap();

	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/projects/{api}"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	// team, project, environment each have a switcher; "Projects" is the sidebar's job here.
	for id in ["crumb-team", "crumb-project", "crumb-environment"] {
		assert_contains(&body, &format!("id=\"{id}\" popover"));
	}
	assert_eq!(body.matches("aria-label=\"Breadcrumb\"").count(), 1);
	let crumbs = &body[body.find("aria-label=\"Breadcrumb\"").unwrap()..];
	let crumbs = &crumbs[..crumbs.find("</nav>").unwrap()];
	assert_not_contains(crumbs, ">Projects</a>");
	// The project switcher lists the siblings, the environment switcher keeps the project.
	assert_contains(
		crumbs,
		&format!(
			"href=\"/teams/{team}/projects/{api}\" class=\"ui-menu-item\" aria-current=\"true\""
		),
	);
	assert_contains(crumbs, "<span class=\"min-w-0 flex-1 truncate\">web</span>");
	assert_contains(
		crumbs,
		&format!("href=\"/teams/{team}/projects/{api}/staging\""),
	);
	// The first environment is in view by default, the chosen one when asked for.
	assert_contains(
		crumbs,
		"<span class=\"ui-crumb-current\" aria-current=\"page\">production</span>",
	);
	for uri in [
		format!("/teams/{team}/projects/{api}?environment={staging}"),
		format!("/teams/{team}/projects/{api}/staging"),
	] {
		let (_, _, body) = get_html(&app, &uri, Some(&cookie)).await;
		assert_contains(
			&body,
			"<span class=\"ui-crumb-current\" aria-current=\"page\">staging</span>",
		);
	}

	// On a page below the project the project and environment are links back.
	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/projects/{api}/apps/new"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "id=\"crumb-environment\" popover");
	assert_contains(&body, "class=\"ui-crumb-link\">production</a>");
}

#[db_test]
async fn another_teams_project_gets_the_404_page_and_no_switchers(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice_token, alice_team) = user_with_team(&app, "alice").await;
	let (bob_token, bob_team) = user_with_team(&app, "bob").await;
	let theirs = project(&app, &bob_token, bob_team, "secret-plan", "secret-plan").await;
	let mine = project(&app, &alice_token, alice_team, "mine", "mine").await;
	let alice = ui_login(&app, "alice").await;

	// Through her own team's path and through his.
	for team in [alice_team, bob_team] {
		let (s, _, body) = get_html(
			&app,
			&format!("/teams/{team}/projects/{theirs}"),
			Some(&alice),
		)
		.await;
		assert_eq!(s, StatusCode::NOT_FOUND, "team {team}");
		assert_not_contains(&body, "secret-plan");
		assert_not_contains(&body, "crumb-environment");
	}
	// Her own project page does not list his projects in its switcher.
	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{alice_team}/projects/{mine}"),
		Some(&alice),
	)
	.await;
	assert_not_contains(&body, "secret-plan");
}

#[db_test]
async fn the_team_switcher_lists_the_users_teams_only(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, alice_team) = user_with_team(&app, "alice").await;
	let (_, bob_team) = user_with_team(&app, "bob").await;
	let alice = ui_login(&app, "alice").await;

	let (_, _, body) = get_html(&app, &format!("/teams/{alice_team}/projects"), Some(&alice)).await;
	assert_contains(
		&body,
		&format!("href=\"/teams/{alice_team}/projects\" class=\"ui-menu-item\""),
	);
	assert_not_contains(&body, &format!("/teams/{bob_team}/"));
	assert_contains(&body, "href=\"/teams/new\"");
}

// --- command palette ------------------------------------------------------------------

#[db_test]
async fn search_finds_the_callers_resources_and_never_another_teams(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice_token, alice_team) = user_with_team(&app, "alice").await;
	let (bob_token, bob_team) = user_with_team(&app, "bob").await;
	let mine = project(&app, &alice_token, alice_team, "orbit-web", "orbit-web").await;
	project(&app, &bob_token, bob_team, "orbit-bobs", "orbit-bobs").await;
	crate::tests::seed_app_and_database(&db, alice_team, mine).await;
	let alice = ui_login(&app, "alice").await;

	let (s, body) = search(&app, &alice, alice_team, "orbit").await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "orbit-web");
	assert_contains(&body, "id=\"palette-list\"");
	assert_not_contains(&body, "orbit-bobs");

	let (_, body) = search(&app, &alice, alice_team, "STORE").await;
	assert_contains(&body, "Storefront");
	assert_contains(&body, "Deploy Storefront");
	assert_contains(
		&body,
		&format!("/teams/{alice_team}/projects/{mine}/apps/5001"),
	);

	// Asking through Bob's team is the 404 page, whatever the query.
	let (s, body) = search(&app, &alice, bob_team, "orbit").await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&body, "orbit-bobs");
	// And without a session there is nothing at all.
	let (s, h, _) = send(
		&app,
		"GET",
		&format!("/teams/{alice_team}/search?q=orbit"),
		None,
		None,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert!(h[header::LOCATION].to_str().unwrap().starts_with("/login"));
}

#[db_test]
async fn search_results_escape_markup_in_names(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let id = project(&app, &token, team, "xss", "<script>alert(1)</script>").await;
	let cookie = ui_login(&app, "alice").await;

	let (s, body) = search(&app, &cookie, team, "script").await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_not_contains(&body, "<script>alert");

	// The query is echoed in the empty state, and escaped there too.
	let (_, body) = search(
		&app,
		&cookie,
		team,
		"%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E",
	)
	.await;
	assert_contains(&body, "No results for");
	assert_not_contains(&body, "<img");
	assert_contains(&body, "&lt;img");

	// And the project page's header, which also names it.
	let (_, _, page) = get_html(&app, &format!("/teams/{team}/projects/{id}"), Some(&cookie)).await;
	assert_not_contains(&page, "<script>alert(1)</script>");
	assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
}

#[db_test]
async fn search_offers_pages_and_actions_the_role_may_use(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, bob, team) = team_with_member(&app).await;

	let (_, body) = search(&app, &alice, team, "").await;
	for text in [
		"Create project",
		"Add server",
		"Add S3 destination",
		"Members",
		"S3 Destinations",
	] {
		assert_contains(&body, text);
	}
	let (_, body) = search(&app, &alice, team, "serv").await;
	assert_contains(&body, "Servers");
	assert_contains(&body, "Add server");
	assert_not_contains(&body, "Members");
	let (_, body) = search(&app, &alice, team, "zzzz-nothing").await;
	assert_contains(&body, "No results for");

	// A plain member reads everything but creates nothing.
	let (s, body) = search(&app, &bob, team, "").await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "Projects");
	for hidden in ["Create project", "Add server", "Add S3 destination"] {
		assert_not_contains(&body, hidden);
	}
	assert_contains(&body, "Create team");
}

// --- settings -------------------------------------------------------------------------

#[db_test]
async fn the_owner_renames_the_team_and_sees_a_message_once(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let page = format!("/teams/{team}/settings");

	let (s, _, body) = get_html(&app, &page, Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(page_main(&body), "Team name");
	assert_contains(page_main(&body), "data-bind:name");
	assert_contains(page_main(&body), "Settings sections");
	assert_contains(&body, "name: 'alice'");

	let (s, h, reply) = post_signals(
		&app,
		&page,
		Some(&alice),
		&json!({ "name": "  Orbit Labs  " }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&reply, &format!("window.location.assign('{page}')"));
	let flash = flash_cookie(&h).expect("a flash cookie");
	assert!(flash.contains("HttpOnly"), "{flash}");
	let flash_pair = flash.split(';').next().unwrap();

	// The next page shows it as a toast, with the new name, and drops the cookie.
	let with_flash = format!("{alice}; {flash_pair}");
	let (s, h, body) = get_html(&app, &page, Some(&with_flash)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "data-flash");
	assert_contains(&body, "Team renamed to Orbit Labs.");
	// (`js_string` writes the space as an escape.)
	assert_contains(&body, "name: 'Orbit\\u0020Labs'");
	assert!(
		h.get_all(header::SET_COOKIE).iter().any(|v| {
			let v = v.to_str().unwrap();
			v.starts_with("flash=;") && v.contains("Max-Age=0")
		}),
		"{h:?}"
	);
	// Without the cookie there is no message.
	let (_, h, body) = get_html(&app, &page, Some(&alice)).await;
	assert_not_contains(&body, "data-flash");
	assert!(h.get_all(header::SET_COOKIE).iter().next().is_none());

	assert_eq!(team_name(&db, team).await, "Orbit Labs");
}

#[db_test]
async fn a_bad_name_is_reported_inline_and_nothing_changes(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let page = format!("/teams/{team}/settings");

	for bad in ["", "   ", &"x".repeat(61)] {
		let (s, h, body) = post_signals(&app, &page, Some(&alice), &json!({ "name": bad })).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Name must be 1 to 60 characters.");
		assert!(flash_cookie(&h).is_none(), "no flash on failure");
	}
	assert_eq!(team_name(&db, team).await, "alice");
}

#[db_test]
async fn a_member_sees_no_rename_form_and_a_forged_post_is_refused(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, bob, team) = team_with_member(&app).await;
	let page = format!("/teams/{team}/settings");

	let (s, _, body) = get_html(&app, &page, Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(page_main(&body), "data-bind:name");
	assert_contains(
		page_main(&body),
		"Only people who can update the team may rename it.",
	);

	let (s, h, body) = post_signals(&app, &page, Some(&bob), &json!({ "name": "Mine now" })).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "You don't have permission to do that.");
	assert!(flash_cookie(&h).is_none());
	assert_eq!(team_name(&db, team).await, "alice");
}

#[db_test]
async fn another_teams_settings_are_the_404_page_and_cannot_be_changed(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, alice_team) = ui_session(&app, "alice").await;
	let (bob, _) = ui_session(&app, "bob").await;
	let theirs = format!("/teams/{alice_team}/settings");

	for path in [
		theirs.clone(),
		format!("{theirs}/registration"),
		format!("{theirs}/appearance"),
	] {
		let (s, _, body) = get_html(&app, &path, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
		assert_not_contains(&body, "alice");
	}
	let (_, _, body) =
		post_signals(&app, &theirs, Some(&bob), &json!({ "name": "Hijacked" })).await;
	assert_not_contains(&body, "window.location");
	assert_eq!(team_name(&db, alice_team).await, "alice");
}

#[db_test]
async fn a_cross_origin_rename_is_refused(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let body = json!({ "name": "Evil" }).to_string();
	let (s, _, _) = send(
		&app,
		"POST",
		&format!("/teams/{team}/settings"),
		Some(&alice),
		Some(&body),
		Some("http://evil.example"),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(team_name(&db, team).await, "alice");
}

#[db_test]
async fn the_registration_page_shows_the_instance_policy_read_only(db: PgPool) {
	let app =
		crate::tests::app_with_registration(db.clone(), crate::state::Registration::Open).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/settings/registration"),
		Some(&alice),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "Open");
	assert_contains(main, "The sign-up page is open right now.");
	assert_contains(main, "REGISTRATION=open");
	assert_not_contains(main, "<form");
	assert_not_contains(main, "<input");
}

#[db_test]
async fn the_appearance_page_marks_the_chosen_theme(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let page = format!("/teams/{team}/settings/appearance");

	let (_, _, body) = get_html(&app, &page, Some(&format!("{alice}; theme=light"))).await;
	let main = page_main(&body);
	assert_contains(main, "name=\"theme\" value=\"light\" aria-pressed=\"true\"");
	assert_contains(main, "name=\"theme\" value=\"dark\" aria-pressed=\"false\"");
	assert_contains(&body, "<html lang=\"en\" class=\"light\">");
}

#[db_test]
async fn the_settings_tabs_cover_members_and_the_token_page(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;

	for (path, current) in [
		(format!("/teams/{team}/settings"), "Team"),
		(format!("/teams/{team}/members"), "Members"),
		(
			format!("/teams/{team}/settings/registration"),
			"Registration",
		),
		(
			format!("/teams/{team}/models/settings"),
			"Hugging Face token",
		),
		(format!("/teams/{team}/settings/appearance"), "Appearance"),
	] {
		let (s, _, body) = get_html(&app, &path, Some(&alice)).await;
		assert_eq!(s, StatusCode::OK, "{path}");
		let main = page_main(&body);
		assert_contains(main, "aria-label=\"Settings sections\"");
		assert_contains(main, &format!("aria-current=\"page\">{current}</a>"));
		assert_eq!(main.matches("aria-current=\"page\"").count(), 1, "{path}");
	}
}

#[db_test]
async fn creating_a_team_leaves_a_message_for_its_first_page(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, _) = ui_session(&app, "alice").await;
	let (s, h, body) = post_signals(
		&app,
		"/teams/new",
		Some(&cookie),
		&json!({ "name": "Second", "slug": "second" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "window.location.assign('/teams/");
	let flash = flash_cookie(&h).expect("a flash cookie");
	assert_contains(&flash, "Second");
}
