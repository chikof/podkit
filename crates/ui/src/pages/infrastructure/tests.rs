//! Tests of the Infrastructure pages: what the overview shows, what the setup derives from
//! the team's own data, who may skip or dismiss, and that nothing crosses a team.

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, send, ui_login,
	ui_session,
};
use crate::tests::infrastructure_ui::{
	grant, seed_estate, seed_git_provider, seed_registry, seed_scan_with_database, set_scanning,
	set_server_status, stored_setup,
};
use crate::tests::{
	PgPool, app, call, db_test, join_as_project_reader, member_role, revoke_permission, seed_key,
	server_id, state, user_id, user_with_team,
};
use podkit_core::ids::{TeamId, UserId};
use podkit_core::infrastructure::service::{self, SetupView};
use podkit_core::infrastructure::setup::{State, Step};

const MEMBER: i64 = 2;

fn setup_url(team: i64) -> String {
	format!("/teams/{team}/infrastructure/setup")
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

fn state_of(view: &SetupView, step: Step) -> Option<State> {
	view.steps.iter().find(|s| s.step == step).map(|s| s.state)
}

async fn view(db: &PgPool, user: &str, team: i64) -> SetupView {
	let uid = UserId(user_id(db, user).await);
	service::setup(&state(db.clone()), uid, TeamId(team))
		.await
		.unwrap()
}

// --- the checklist is derived from what exists -----------------------------------------

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_checklist_follows_the_teams_own_data(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;

	// A new team: the local server answers, everything else is still to do. Scanning is on
	// by default, but nothing has been scanned, so the database is not downloaded.
	let v = view(&db, "alice", team).await;
	assert_eq!(v.steps.len(), 6);
	assert_eq!(state_of(&v, Step::Server), Some(State::Done));
	for step in [
		Step::Proxy,
		Step::Networks,
		Step::Access,
		Step::Security,
		Step::Project,
	] {
		assert_eq!(state_of(&v, step), Some(State::Todo), "{step:?}");
	}
	assert!(!v.complete);
	assert_eq!(v.current, Some(Step::Proxy));
	assert!(v.shown());
	assert_eq!(v.done_count(), 1);

	// Two environments with resources, a running application and a shared network.
	seed_estate(&db, team).await;
	let v = view(&db, "alice", team).await;
	assert_eq!(state_of(&v, Step::Proxy), Some(State::Done));
	assert_eq!(state_of(&v, Step::Networks), Some(State::Done));
	assert_eq!(state_of(&v, Step::Project), Some(State::Done));
	assert_eq!(state_of(&v, Step::Access), Some(State::Todo));

	// One kind of access is not enough for the step; all three settle it.
	seed_key(&db, team, "laptop").await;
	assert_eq!(
		state_of(&view(&db, "alice", team).await, Step::Access),
		Some(State::Todo)
	);
	seed_registry(&db, team).await;
	seed_git_provider(&db, team).await;
	assert_eq!(
		state_of(&view(&db, "alice", team).await, Step::Access),
		Some(State::Done)
	);

	// Scanning needs the switch and a downloaded database.
	set_scanning(&db, team, false).await;
	assert_eq!(
		state_of(&view(&db, "alice", team).await, Step::Security),
		Some(State::Todo)
	);
	set_scanning(&db, team, true).await;
	let server = server_id(&db, team).await;
	seed_scan_with_database(&db, team, server).await;
	let v = view(&db, "alice", team).await;
	assert_eq!(state_of(&v, Step::Security), Some(State::Done));
	assert!(v.complete, "{:?}", v.steps);
	assert!(!v.shown(), "a finished checklist leaves the overview");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_server_that_does_not_answer_keeps_the_server_step_open(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	set_server_status(&db, team, "unreachable").await;
	let v = view(&db, "alice", team).await;
	assert_eq!(state_of(&v, Step::Server), Some(State::Todo));
	assert!(v.steps[0].checks[0].detail.contains("not answering"));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn another_teams_data_is_never_counted(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, alice_team) = user_with_team(&app, "alice").await;
	let (_, bob_team) = user_with_team(&app, "bob").await;
	seed_estate(&db, bob_team).await;
	seed_registry(&db, bob_team).await;
	let v = view(&db, "alice", alice_team).await;
	for step in [Step::Proxy, Step::Networks, Step::Project, Step::Access] {
		assert_eq!(state_of(&v, step), Some(State::Todo), "{step:?}");
	}
}

// --- who sees which step ---------------------------------------------------------------

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_project_reader_sees_only_the_project_step_and_no_overview(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	register(&app, "carol").await;
	let carol = user_id(&db, "carol").await;
	join_as_project_reader(&db, team, carol, 9001).await;
	let cookie = ui_login(&app, "carol").await;

	let (s, _, body) = get_html(&app, &setup_url(team), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "First project");
	for hidden in ["Proxy", "Networks", "Access", "Security", ">Server<"] {
		assert_not_contains(main, hidden);
	}
	assert_eq!(body.matches("<h1").count(), 1);

	// Asking for a step that is not theirs is a 404, as for any page that is not theirs.
	let (s, _, _) = get_html(
		&app,
		&format!("{}?step=proxy", setup_url(team)),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);

	// The overview needs `read server`.
	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/infrastructure"),
		Some(&cookie),
	)
	.await;
	assert!(
		s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
		"{s}"
	);
	assert_not_contains(&body, "What runs where");
	// And the sidebar lists no Infrastructure section for them.
	let (_, _, projects) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	assert_not_contains(&projects, "id=\"nav-infrastructure\"");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_step_about_something_the_role_cannot_read_is_left_out(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let role = member_role(&db, team, "alice").await;

	let (_, _, body) = get_html(&app, &setup_url(team), Some(&cookie)).await;
	for step in [
		"Server",
		"Proxy",
		"Networks",
		"Access",
		"Security",
		"First project",
	] {
		assert_contains(page_main(&body), step);
	}

	revoke_permission(&db, role, "read", "scan").await;
	let (_, _, body) = get_html(&app, &setup_url(team), Some(&cookie)).await;
	assert_not_contains(page_main(&body), "Security");
	assert_contains(page_main(&body), "Access");

	for resource in ["ssh_key", "git_provider", "registry"] {
		revoke_permission(&db, role, "read", resource).await;
	}
	let (_, _, body) = get_html(&app, &setup_url(team), Some(&cookie)).await;
	assert_not_contains(page_main(&body), "Access");
	assert_contains(page_main(&body), "Networks");

	// One kind of access back: the step returns with that line alone.
	grant(&db, role, "read", "registry").await;
	let (_, _, body) = get_html(
		&app,
		&format!("{}?step=access", setup_url(team)),
		Some(&cookie),
	)
	.await;
	let main = page_main(&body);
	assert_contains(main, "Registry");
	assert_not_contains(main, "Git provider");
	assert_not_contains(main, "SSH key");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_sidebar_section_lists_only_what_the_role_reads(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let role = member_role(&db, team, "alice").await;
	revoke_permission(&db, role, "read", "ssh_key").await;
	revoke_permission(&db, role, "read", "registry").await;
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	for shown in [
		"Overview",
		"Servers",
		"Networks",
		"Git providers",
		"Security",
		"S3 destinations",
	] {
		assert_contains(
			&body,
			&format!("<span class=\"ui-nav-label\">{shown}</span>"),
		);
	}
	for hidden in ["Keys", "Registries"] {
		assert_not_contains(
			&body,
			&format!("<span class=\"ui-nav-label\">{hidden}</span>"),
		);
	}
}

// --- skip and dismiss ------------------------------------------------------------------

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn skipping_and_dismissing_are_stored_for_this_team_only(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let (_, other) = user_with_team(&app, "dave").await;
	assert!(stored_setup(&db, team).await.is_none());

	let (s, _, _) = post_signals(
		&app,
		&format!("{}/skip/networks", setup_url(team)),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		stored_setup(&db, team).await,
		Some((false, vec!["networks".to_owned()]))
	);
	let (_, _, page) = get_html(
		&app,
		&format!("{}?step=networks", setup_url(team)),
		Some(&alice),
	)
	.await;
	assert_contains(&page, "Skipped");
	assert_contains(&page, "Undo skip");

	// A skip is taken back, and a second skip does not duplicate the entry.
	for _ in 0..2 {
		post_signals(
			&app,
			&format!("{}/skip/ssh_key", setup_url(team)),
			Some(&alice),
			&json!({}),
		)
		.await;
	}
	assert_eq!(
		stored_setup(&db, team).await,
		Some((false, vec!["networks".to_owned(), "ssh_key".to_owned()]))
	);
	post_signals(
		&app,
		&format!("{}/unskip/networks", setup_url(team)),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_eq!(
		stored_setup(&db, team).await,
		Some((false, vec!["ssh_key".to_owned()]))
	);

	// Dismissing hides the checklist from the overview; the setup page stays reachable and
	// offers to bring it back.
	let overview = format!("/teams/{team}/infrastructure");
	let (_, _, before) = get_html(&app, &overview, Some(&alice)).await;
	assert_contains(&before, "Set up your infrastructure");
	post_signals(
		&app,
		&format!("{}/dismiss", setup_url(team)),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_eq!(stored_setup(&db, team).await.map(|s| s.0), Some(true));
	let (_, _, after) = get_html(&app, &overview, Some(&alice)).await;
	assert_not_contains(page_main(&after), "Set up your infrastructure");
	assert_contains(page_main(&after), ">Setup<");
	let (_, _, page) = get_html(&app, &setup_url(team), Some(&alice)).await;
	assert_contains(&page, "Show on the overview again");
	post_signals(
		&app,
		&format!("{}/reopen", setup_url(team)),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_eq!(stored_setup(&db, team).await.map(|s| s.0), Some(false));

	// Nothing leaked to the other team.
	assert!(stored_setup(&db, other).await.is_none());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_plain_member_cannot_skip_or_dismiss_even_with_a_forged_post(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, bob, team) = team_with_member(&app).await;

	// The member sees the setup but none of the buttons that change it.
	let (_, _, page) = get_html(
		&app,
		&format!("{}?step=networks", setup_url(team)),
		Some(&bob),
	)
	.await;
	assert_not_contains(page_main(&page), "/skip/");
	assert_not_contains(page_main(&page), ">Dismiss<");
	let (_, _, overview) =
		get_html(&app, &format!("/teams/{team}/infrastructure"), Some(&bob)).await;
	assert_contains(page_main(&overview), "Continue setup");
	assert_not_contains(page_main(&overview), ">Dismiss<");

	for path in [
		"skip/networks",
		"skip/registry",
		"dismiss",
		"reopen",
		"unskip/networks",
	] {
		let (s, _, body) = post_signals(
			&app,
			&format!("{}/{path}", setup_url(team)),
			Some(&bob),
			&json!({}),
		)
		.await;
		assert!(
			s == StatusCode::FORBIDDEN || body.contains("_error"),
			"{path}: {s} {body}"
		);
	}
	assert!(
		stored_setup(&db, team).await.is_none(),
		"nothing was stored"
	);

	// An item that is not one of the four is refused for the owner too.
	let (s, _, body) = post_signals(
		&app,
		&format!("{}/skip/everything", setup_url(team)),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert!(
		s == StatusCode::NOT_FOUND || body.contains("_error"),
		"{s} {body}"
	);
	assert!(stored_setup(&db, team).await.is_none());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_cross_origin_dismiss_is_refused(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let (s, _, _) = send(
		&app,
		"POST",
		&format!("{}/dismiss", setup_url(team)),
		Some(&alice),
		Some("{}"),
		Some("http://evil.example"),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert!(stored_setup(&db, team).await.is_none());
}

// --- another team's pages are not found ------------------------------------------------

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn another_teams_infrastructure_is_a_404_and_cannot_be_changed(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = ui_session(&app, "alice").await;
	seed_estate(&db, team).await;
	let (mallory, _) = ui_session(&app, "mallory").await;

	for path in [
		format!("/teams/{team}/infrastructure"),
		format!("/teams/{team}/infrastructure/networks"),
		setup_url(team),
		format!("{}?step=server", setup_url(team)),
	] {
		let (s, _, body) = get_html(&app, &path, Some(&mallory)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
		for secret in ["Storefront", "backplane", "Shop"] {
			assert_not_contains(&body, secret);
		}
	}
	for path in ["skip/networks", "dismiss", "reopen"] {
		let (s, _, body) = post_signals(
			&app,
			&format!("{}/{path}", setup_url(team)),
			Some(&mallory),
			&json!({}),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{path}");
		assert_contains(&body, "That no longer exists.");
	}
	assert!(stored_setup(&db, team).await.is_none());
}

// --- the pages -------------------------------------------------------------------------

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_overview_says_what_runs_where_and_escapes_every_name(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let estate = seed_estate(&db, team).await;

	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/infrastructure"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_eq!(body.matches("<h1").count(), 1, "one h1");
	assert_contains(main, "<h1>Infrastructure</h1>");

	// The server: its proxy, networks with member counts and scanner.
	assert_contains(main, "Traefik running");
	assert_contains(main, "HTTP :8088");
	assert_contains(main, "HTTPS off");
	assert_contains(main, "production net");
	assert_contains(main, "backplane");
	assert_contains(main, "staging net");
	assert_contains(main, "Scanning on");

	// Resources grouped by project and environment, markup escaped.
	assert_contains(main, "Shop &lt;i&gt;x&lt;/i&gt;");
	assert_not_contains(&body, "<i>x</i>");
	assert_not_contains(&body, "<script>alert(1)</script>");
	assert_contains(main, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_contains(main, "production");
	assert_contains(main, "staging");
	for name in ["Storefront", "orders"] {
		assert_contains(main, name);
	}
	assert_contains(
		main,
		&format!(
			"/teams/{team}/projects/{}/apps/{}",
			estate.project, estate.storefront
		),
	);

	// The table: resource, server, networks, address.
	let table = main
		.split("id=\"what-runs-where\"")
		.nth(1)
		.expect("the table");
	for header in ["Resource", "Server", "Networks", "Address"] {
		assert_contains(table, header);
	}
	assert_contains(table, "storefront.127-0-0-1.sslip.io");
	assert_contains(table, "production net, backplane");
	assert_contains(table, "Internal only");
	assert_contains(
		table,
		"Application · Shop &lt;i&gt;x&lt;/i&gt; / production",
	);
	assert_contains(table, "Database · Shop &lt;i&gt;x&lt;/i&gt; / production");

	// Someone who may not read databases sees the rest, never an error.
	let role = member_role(&db, team, "alice").await;
	revoke_permission(&db, role, "read", "database").await;
	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/infrastructure"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(page_main(&body), "orders");
	assert_contains(page_main(&body), "Storefront");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_first_run_overview_is_empty_and_leads_to_the_setup(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/infrastructure"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "Nothing runs yet");
	assert_contains(main, "Nothing runs here yet.");
	assert_contains(main, "Set up your infrastructure");
	assert_contains(main, "1 of 6 steps done");
	assert_contains(main, "Continue setup");
	assert_contains(main, &format!("{}?step=proxy", setup_url(team)));
	// The local server is there, checked and active.
	assert_contains(main, "Local");
	assert_contains(main, "Active");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn every_setup_step_renders_with_its_live_state(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	for (slug, label) in [
		("server", "Server"),
		("proxy", "Proxy"),
		("networks", "Networks"),
		("access", "Access"),
		("security", "Security"),
		("project", "First project"),
	] {
		let (s, _, body) = get_html(
			&app,
			&format!("{}?step={slug}", setup_url(team)),
			Some(&cookie),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{slug}");
		assert_eq!(body.matches("<h1").count(), 1, "{slug}");
		assert_eq!(body.matches("aria-current=\"step\"").count(), 1, "{slug}");
		let main = page_main(&body);
		assert_contains(main, "ui-steps");
		assert_contains(main, &format!("{label}<span class=\"ui-tag"));
		assert!(main.contains("Done") || main.contains("To do"), "{slug}");
	}
	let (_, _, body) = get_html(
		&app,
		&format!("{}?step=proxy", setup_url(team)),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, ":8088");
	assert_contains(&body, "Closed: no ACME email is set");
	assert_contains(&body, "ACME_EMAIL");
	let (_, _, body) = get_html(
		&app,
		&format!("{}?step=server", setup_url(team)),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, "1 server is answering.");
	assert_contains(&body, "Add a server");
	let (_, _, body) = get_html(
		&app,
		&format!("{}?step=access", setup_url(team)),
		Some(&cookie),
	)
	.await;
	for line in ["SSH key", "Git provider", "Registry"] {
		assert_contains(&body, line);
	}
	assert_contains(&body, "/skip/ssh_key");
	let (s, _, _) = get_html(
		&app,
		&format!("{}?step=nope", setup_url(team)),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// With no step asked for, the first one still to do opens.
	let (_, _, body) = get_html(&app, &setup_url(team), Some(&cookie)).await;
	assert_contains(&body, "Proxy<span class=\"ui-tag");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_networks_page_lists_this_teams_networks_only(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (_, other) = user_with_team(&app, "bob").await;
	seed_estate(&db, team).await;
	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/infrastructure/networks"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body.matches("<h1").count(), 1);
	let main = page_main(&body);
	for network in ["production net", "staging net", "backplane"] {
		assert_contains(main, network);
	}
	assert_contains(main, "Manage");
	let bob = ui_login(&app, "bob").await;
	let (_, _, theirs) = get_html(
		&app,
		&format!("/teams/{other}/infrastructure/networks"),
		Some(&bob),
	)
	.await;
	assert_not_contains(&theirs, "backplane");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_empty_projects_page_offers_the_setup_until_it_is_dismissed(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let projects = format!("/teams/{team}/projects");

	let (_, _, body) = get_html(&app, &projects, Some(&cookie)).await;
	assert_contains(page_main(&body), "First time here?");
	assert_contains(page_main(&body), &setup_url(team));

	post_signals(
		&app,
		&format!("{}/dismiss", setup_url(team)),
		Some(&cookie),
		&json!({}),
	)
	.await;
	let (_, _, body) = get_html(&app, &projects, Some(&cookie)).await;
	assert_not_contains(page_main(&body), "First time here?");
	assert_contains(page_main(&body), "No projects yet");

	// A team that already has a project is past its first run.
	post_signals(
		&app,
		&format!("{}/reopen", setup_url(team)),
		Some(&cookie),
		&json!({}),
	)
	.await;
	seed_estate(&db, team).await;
	let (_, _, body) = get_html(&app, &projects, Some(&cookie)).await;
	assert_not_contains(page_main(&body), "First time here?");
}
