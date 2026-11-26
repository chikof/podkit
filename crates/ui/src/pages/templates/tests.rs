//! Tests of the template gallery, the detail page and the deploy dialog.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login, ui_session,
};
use crate::tests::grid::env_id;
use crate::tests::stacks_ui::stack_environment;
use crate::tests::{
	PgPool, call, count, db_test, local_server_id, only_id, project_id, user_with_team,
};
use podkit_core::templates::{self, Variable};

/// Percent-encodes a Datastar signal object for the `?datastar=` query of a GET.
fn signals_query(signals: &Value) -> String {
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

struct Fixture {
	app: Router,
	state: podkit_core::state::AppState,
	cookie: String,
	team: i64,
	project: i64,
	environment: i64,
	server: i64,
}

impl Fixture {
	async fn new(db: &PgPool) -> Self {
		let state = crate::tests::state(db.clone());
		let app = crate::routes(state.clone()).await.unwrap();
		let (cookie, team) = ui_session(&app, "alice").await;
		let (s, _, _) = post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			Some(&cookie),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		let project = project_id(db, "shop").await;
		let environment = env_id(db, project, "production").await;
		let server = local_server_id(db, team).await;
		Self {
			app,
			state,
			cookie,
			team,
			project,
			environment,
			server,
		}
	}

	/// Adds the Staging environment through the UI action and returns its id.
	async fn add_staging(&self, db: &PgPool) -> i64 {
		let (s, _, _) = post_signals(
			&self.app,
			&format!(
				"/teams/{}/projects/{}/environments",
				self.team, self.project
			),
			Some(&self.cookie),
			&json!({ "newName": "Staging", "newSlug": "staging" }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		env_id(db, self.project, "staging").await
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	fn gallery(&self) -> String {
		format!("/teams/{}/templates", self.team)
	}

	/// The signals the deploy dialog posts for `id`, with every required answer filled in.
	fn deploy_signals(&self, id: &str) -> Value {
		let t = &templates::find(id).unwrap().template;
		let vars = templates::variables(t);
		let mut s = json!({
			"project": self.project.to_string(),
			"environment": self.environment.to_string(),
			"server": self.server.to_string(),
			"name": "",
		});
		for (i, v) in vars.iter().filter(|v| v.kind != "generated").enumerate() {
			s[format!("v{i}")] = json!(if v.kind == "input" && v.required {
				"answer"
			} else {
				""
			});
		}
		s
	}

	async fn deploy(&self, id: &str) -> String {
		self.deploy_with(id, &self.deploy_signals(id)).await
	}

	async fn deploy_with(&self, id: &str, signals: &Value) -> String {
		let (s, _, body) = post_signals(
			&self.app,
			&format!("/teams/{}/templates/{id}/deploy", self.team),
			Some(&self.cookie),
			signals,
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		body
	}
}

/// A template with the given trait, found in the catalog rather than named, so a catalog
/// refresh does not break the test.
fn find(pred: impl Fn(&templates::Loaded) -> bool) -> std::sync::Arc<templates::Loaded> {
	templates::load_all()
		.templates
		.values()
		.find(|l| pred(l))
		.cloned()
		.expect("a template with that trait")
}

fn cards(body: &str) -> usize {
	body.matches("ui-link-card-link").count()
}

fn redirect_of(body: &str) -> String {
	body.split("assign('")
		.nth(1)
		.and_then(|s| s.split('\'').next())
		.unwrap_or_else(|| panic!("no redirect in {body}"))
		.to_owned()
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_flagged_template_is_labelled_in_the_gallery_and_explained_on_its_page(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (_, list) = f.get(&format!("{}?q=bytebase", f.gallery())).await;
	assert_contains(page_main(&list), "Broken");
	let (_, list) = f.get(&format!("{}?q=gotify", f.gallery())).await;
	assert_not_contains(page_main(&list), "Broken");

	let (_, page) = f.get(&format!("{}/bytebase", f.gallery())).await;
	assert_contains(
		page_main(&page),
		"The image tag it names is no longer published.",
	);
	assert_contains(page_main(&page), "Dokploy community");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_market_link_only_leads_to_a_template_of_this_catalog_and_deploys_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (status, headers, _) = get_html(&f.app, "/market/gotify", Some(&f.cookie)).await;
	assert_eq!(status, StatusCode::SEE_OTHER);
	assert_eq!(
		headers["location"],
		format!("/teams/{}/templates/gotify", f.team)
	);
	assert_eq!(count(&db, "applications").await, 0);
	assert_eq!(count(&db, "stacks").await, 0);

	// An id the catalog does not have is answered with a page, and is not echoed into a link.
	let (status, _, body) = get_html(&f.app, "/market/no-such%2F..%2Fx", Some(&f.cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "This podkit does not have that template");
	assert_not_contains(&body, "no-such");

	// Without a session it is the sign-in page's business.
	let (status, _, _) = get_html(&f.app, "/market/gotify", None).await;
	assert!(status.is_redirection(), "{status}");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_gallery_shows_a_page_of_logo_cards_with_ai_and_databases_to_filter_by(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, body) = f.get(&f.gallery()).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "<h1");
	assert_contains(main, "Search templates");
	assert_contains(main, "data-grid-nav");
	assert_contains(main, "aria-label=\"Categories\"");
	assert_contains(main, "<span>AI</span>");
	assert_contains(main, "<span>Databases</span>");
	assert_eq!(cards(main), 24, "one page of 24 rows");
	// Logos are the template's own file, served from this origin, and the rows below the
	// first screen lazy-load.
	assert_contains(main, "src=\"/static/logos/");
	assert_contains(main, "loading=\"lazy\"");
	assert_contains(main, "width=\"40\" height=\"40\"");
	let total = templates::search(None, None).len();
	assert_contains(main, &format!("{total} in the catalog"));
	assert_contains(main, "24 templates shown");
	assert_contains(main, "Page 1 of ");
	// A row is small: the gallery page stays well under the 93 KB it was with every card.
	assert!(body.len() < 50_000, "{} bytes", body.len());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn search_matches_the_catalog_only_and_escapes_what_was_typed(db: PgPool) {
	let f = Fixture::new(&db).await;

	let (_, body) = f.get(&format!("{}?q=gotify", f.gallery())).await;
	let main = page_main(&body);
	assert_contains(main, "Gotify");
	assert_eq!(cards(main), templates::search(Some("gotify"), None).len());
	assert_not_contains(main, "Umami");

	// Hostile text is an ordinary query: nothing matches, and it is echoed escaped.
	let (s, body) = f
		.get(&format!(
			"{}?q=%3Cscript%3Ealert(1)%3C%2Fscript%3E",
			f.gallery()
		))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "No template matches");
	assert_not_contains(&body, "<script>alert(1)");
	assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");

	// SQL-ish and path-ish text is just text too.
	for q in [
		"%27%3B%20DROP%20TABLE%20templates%3B--",
		"%25zz",
		"..%2F..%2Fetc",
	] {
		let (s, body) = f.get(&format!("{}?q={q}", f.gallery())).await;
		assert_eq!(s, StatusCode::OK, "{q}");
		assert_contains(&body, "No template matches");
	}
	assert_eq!(count(&db, "projects").await, 1, "nothing was written");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_debounced_search_answers_with_the_results_region_only(db: PgPool) {
	let f = Fixture::new(&db).await;
	let uri = format!(
		"{}/results?{}",
		f.gallery(),
		signals_query(&json!({ "q": "gotify", "category": "" }))
	);
	let (s, body) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "datastar-patch-elements");
	assert_contains(&body, "id=\"tpl-browser\"");
	assert_contains(&body, "id=\"tpl-count\"");
	assert_contains(&body, "Gotify");
	assert_not_contains(&body, "<main");
	// The page's search box wires the same fragment, debounced.
	let (_, page) = f.get(&f.gallery()).await;
	assert_contains(&page, "data-on:input__debounce.300ms");
	assert_contains(&page, &format!("/teams/{}/templates/results", f.team));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_category_narrows_the_grid_and_an_unknown_one_is_ignored(db: PgPool) {
	let f = Fixture::new(&db).await;
	let in_db = templates::search(None, Some("database"));
	assert!(!in_db.is_empty());
	let (_, body) = f.get(&format!("{}?category=database", f.gallery())).await;
	let main = page_main(&body);
	assert_eq!(cards(main), in_db.len().min(24));
	assert_contains(main, "aria-current=\"page\"");
	assert_not_contains(main, "Gotify");

	let (s, body) = f
		.get(&format!("{}?category=%3Cb%3E&page=999999", f.gallery()))
		.await;
	assert_eq!(s, StatusCode::OK);
	let all = templates::search(None, None).len();
	let last_page = all - (all.div_ceil(24) - 1) * 24;
	assert_eq!(
		cards(page_main(&body)),
		last_page,
		"the last page, not an empty one"
	);
	assert_not_contains(&body, "<b>");

	let (_, second) = f.get(&format!("{}?page=2", f.gallery())).await;
	assert_contains(page_main(&second), "Page 2 of ");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_gallery_is_for_members_only_and_another_teams_pages_are_404(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, _, _) = get_html(&f.app, &f.gallery(), None).await;
	assert_eq!(s, StatusCode::SEE_OTHER, "signed out goes to sign in");

	let (_, mallory_team) = user_with_team(&f.app, "mallory").await;
	let mallory = ui_login(&f.app, "mallory").await;
	for uri in [
		f.gallery(),
		format!("{}/gotify", f.gallery()),
		format!("{}/results", f.gallery()),
		format!("/teams/{}/projects/{}/templates", f.team, f.project),
	] {
		let (s, _, body) = get_html(&f.app, &uri, Some(&mallory)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_not_contains(&body, "Gotify");
	}

	// Another team's project is not found through your own team either.
	for tail in ["templates", "templates/gotify"] {
		let (s, _, _) = get_html(
			&f.app,
			&format!("/teams/{mallory_team}/projects/{}/{tail}", f.project),
			Some(&mallory),
		)
		.await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{tail}");
	}
	// Deploying into it is refused the same way.
	let mut signals = f.deploy_signals("gotify");
	signals["server"] = json!(local_server_id(&db, mallory_team).await.to_string());
	let (_, _, body) = post_signals(
		&f.app,
		&format!("/teams/{mallory_team}/templates/gotify/deploy"),
		Some(&mallory),
		&signals,
	)
	.await;
	assert_not_contains(&body, "assign");
	assert_eq!(count(&db, "applications").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_detail_lists_services_and_questions_and_labels_generated_values(db: PgPool) {
	let f = Fixture::new(&db).await;
	let l = find(|l| {
		l.template
			.variables
			.values()
			.any(|v| matches!(v, Variable::Expr(e) if e.contains("${password")))
	});
	let id = &l.template.id;
	let (s, body) = f.get(&format!("{}/{id}", f.gallery())).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, &l.template.name);
	assert_contains(main, "Services");
	assert_contains(main, &l.services[0].image);
	assert_contains(main, "Requirements");
	assert_contains(main, "Variables");
	assert_contains(main, ">generated<");
	assert_contains(main, "data-kind=\"generated\"");
	// What the template computes is never on the page: not its expressions, not a value.
	assert_not_contains(main, "${");
	assert_contains(main, "id=\"tpl-deploy\"");
	assert_contains(main, "command=\"show-modal\" commandfor=\"tpl-deploy\"");

	let (s, _) = f.get(&format!("{}/no-such-template", f.gallery())).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_deploy_dialog_asks_where_and_what_and_binds_every_answer(db: PgPool) {
	let f = Fixture::new(&db).await;
	let l = find(|l| {
		l.template
			.variables
			.values()
			.any(|v| matches!(v, Variable::Input(i) if !i.secret))
	});
	let id = &l.template.id;
	let (_, body) = f.get(&format!("{}/{id}?deploy=1", f.gallery())).await;
	let main = page_main(&body);
	for field in [
		"data-bind:project",
		"data-bind:environment",
		"data-bind:server",
		"data-bind:name",
		"data-bind=\"v0\"",
	] {
		assert_contains(main, field);
	}
	assert_contains(main, "Shop");
	assert_contains(main, "production");
	// Opens itself when asked to, and closes on Esc, as any modal dialog.
	assert_contains(main, "showModal()");
	assert_contains(main, "closedby=\"any\"");
	assert_contains(main, "aria-labelledby=\"tpl-deploy-title\"");
	assert_contains(main, &format!("/teams/{}/templates/{id}/deploy", f.team));
	// Changing the project fetches that project's environments.
	assert_contains(
		main,
		&format!("/teams/{}/templates/{id}/environments", f.team),
	);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_project_entrance_preselects_the_project_and_keeps_the_environment(db: PgPool) {
	let f = Fixture::new(&db).await;
	let staging = f.add_staging(&db).await;
	let (s, body) = f
		.get(&format!(
			"/teams/{}/projects/{}/templates/gotify?environment={staging}",
			f.team, f.project
		))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(
		&body,
		&format!("&quot;environment&quot;:&quot;{staging}&quot;"),
	);
	assert_contains(
		&body,
		&format!("&quot;project&quot;:&quot;{}&quot;", f.project),
	);

	let (_, gallery) = f
		.get(&format!(
			"/teams/{}/projects/{}/templates?environment={staging}",
			f.team, f.project
		))
		.await;
	assert_contains(
		&gallery,
		&format!(
			"href=\"/teams/{}/projects/{}/templates/{}?environment={staging}\"",
			f.team,
			f.project,
			first_template(&gallery)
		),
	);
}

fn first_template(body: &str) -> String {
	let rest = &body[body.find("ui-link-card-link").unwrap()..];
	let at = rest.find("/templates/").unwrap() + "/templates/".len();
	rest[at..]
		.chars()
		.take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
		.collect()
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn deploying_a_single_service_template_creates_an_application_and_opens_its_log(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f.deploy("gotify").await;
	assert_eq!(count(&db, "applications").await, 1);
	assert_eq!(count(&db, "stacks").await, 0);
	let target = redirect_of(&body);
	assert!(
		target.starts_with(&format!("/teams/{}/projects/{}/apps/", f.team, f.project)),
		"{target}"
	);
	assert!(target.contains("/deployments/"), "{target}");
	// The page it leads to has the live log.
	let (s, page) = f.get(&target).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "build-log");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn deploying_a_multi_service_template_creates_a_stack_and_opens_its_log(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f.deploy("umami").await;
	assert_eq!(count(&db, "stacks").await, 1);
	assert_eq!(count(&db, "applications").await, 0);
	let target = redirect_of(&body);
	assert!(
		target.starts_with(&format!("/teams/{}/projects/{}/stacks/", f.team, f.project)),
		"{target}"
	);
	assert!(target.ends_with("/deployments"), "{target}");
	let (s, page) = f.get(&target).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Umami");
	assert_contains(&page, "deploy-log");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_deploy_into_the_chosen_environment_lands_there(db: PgPool) {
	let f = Fixture::new(&db).await;
	let staging = f.add_staging(&db).await;
	let mut signals = f.deploy_signals("umami");
	signals["environment"] = json!(staging.to_string());
	f.deploy_with("umami", &signals).await;
	assert_eq!(stack_environment(&db).await, staging);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_bad_deploy_says_what_is_wrong_inline_and_creates_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let mut no_project = f.deploy_signals("gotify");
	no_project["project"] = json!("");
	assert_contains(
		&f.deploy_with("gotify", &no_project).await,
		"Pick a project.",
	);
	let mut no_server = f.deploy_signals("gotify");
	no_server["server"] = json!("x");
	assert_contains(&f.deploy_with("gotify", &no_server).await, "Pick a server.");
	let mut other = f.deploy_signals("gotify");
	other["server"] = json!("999999");
	let body = f.deploy_with("gotify", &other).await;
	assert_contains(&body, "_error");
	assert_not_contains(&body, "assign");

	// A template that asks for an answer refuses an empty one.
	let l = find(|l| {
		l.template
			.variables
			.values()
			.any(|v| matches!(v, Variable::Input(i) if i.default.is_none()))
	});
	let mut blank = f.deploy_signals(&l.template.id);
	for (k, v) in blank.as_object_mut().unwrap() {
		if k.starts_with('v') {
			*v = json!("");
		}
	}
	let body = f.deploy_with(&l.template.id, &blank).await;
	assert_not_contains(&body, "assign");

	let (_, _, body) = post_signals(
		&f.app,
		&format!("/teams/{}/templates/no-such/deploy", f.team),
		Some(&f.cookie),
		&f.deploy_signals("gotify"),
	)
	.await;
	assert_not_contains(&body, "assign");
	assert_eq!(count(&db, "applications").await, 0);
	assert_eq!(count(&db, "stacks").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_environments_of_the_chosen_project_come_back_for_the_select(db: PgPool) {
	let f = Fixture::new(&db).await;
	let uri = format!(
		"/teams/{}/templates/gotify/environments?{}",
		f.team,
		signals_query(&json!({ "project": f.project.to_string() }))
	);
	let (s, body) = f.get(&uri).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "id=\"tpl-env-field\"");
	assert_contains(&body, "production");
	assert_contains(&body, &format!("\"environment\":\"{}\"", f.environment));
	let (_, other) = f
		.get(&format!(
			"/teams/{}/templates/gotify/environments?{}",
			f.team,
			signals_query(&json!({ "project": "999999" }))
		))
		.await;
	assert_not_contains(&other, "production");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn generated_secrets_are_never_on_any_page(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f.deploy("umami").await;
	let stack = only_id(&db, "stacks").await;
	let secret = crate::tests::stacks_ui::stack_variable(&f.state, stack, "DB_PASSWORD")
		.await
		.expect("the template generated a database password");
	assert!(secret.len() >= 16);
	assert_not_contains(&body, &secret);
	let base = format!("/teams/{}/projects/{}/stacks/{stack}", f.team, f.project);
	for tab in [
		"",
		"/compose",
		"/env",
		"/domains",
		"/deployments",
		"/logs",
		"/advanced",
	] {
		let (s, page) = f.get(&format!("{base}{tab}")).await;
		assert_eq!(s, StatusCode::OK, "{tab}");
		assert_not_contains(&page, &secret);
	}
	let (_, detail) = f.get(&format!("{}/umami", f.gallery())).await;
	assert_not_contains(&detail, &secret);
	let (_, env_page) = f.get(&format!("{base}/env")).await;
	assert_contains(&env_page, "DB_PASSWORD");
	assert_contains(&env_page, "value hidden");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_credential_the_template_reveals_is_in_the_deploy_answer_and_nowhere_else(db: PgPool) {
	let f = Fixture::new(&db).await;
	let l = find(|l| !l.template.reveal.is_empty());
	let id = l.template.id.clone();
	let body = f.deploy(&id).await;
	assert_not_contains(&body, "window.location.assign");
	assert_contains(&body, "podkit shows them only now");
	assert_contains(&body, &l.template.reveal[0]);
	let value = body
		.split("readonly value=\"")
		.nth(1)
		.and_then(|s| s.split('"').next())
		.expect("a credential field")
		.to_owned();
	assert!(value.len() >= 8, "{value}");

	let base = if count(&db, "stacks").await == 1 {
		format!(
			"/teams/{}/projects/{}/stacks/{}",
			f.team,
			f.project,
			only_id(&db, "stacks").await
		)
	} else {
		format!(
			"/teams/{}/projects/{}/apps/{}",
			f.team,
			f.project,
			only_id(&db, "applications").await
		)
	};
	for uri in [
		base.clone(),
		format!("{base}/env"),
		format!("{base}/deployments"),
		format!("{}/{id}", f.gallery()),
		format!("/teams/{}/projects/{}", f.team, f.project),
	] {
		let (_, page) = f.get(&uri).await;
		assert_not_contains(&page, &value);
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_member_who_may_not_create_sees_no_deploy_button_and_a_forged_post_is_refused(
	db: PgPool,
) {
	let f = Fixture::new(&db).await;
	// Carol joins with the built-in read-only Member role.
	let creds = json!({ "email": "alice@example.com", "password": "correct horse battery" });
	let (_, login) = call(&f.app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
	let token = login["token"].as_str().unwrap().to_owned();
	user_with_team(&f.app, "carol").await;
	let (s, _) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{}/members", f.team),
		Some(&token),
		Some(json!({ "email": "carol@example.com", "role_id": 2 })),
	)
	.await;
	assert!(s.is_success());
	let carol = ui_login(&f.app, "carol").await;

	for id in ["gotify", "umami"] {
		let (s, _, page) = get_html(&f.app, &format!("{}/{id}", f.gallery()), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK, "{id}");
		let main = page_main(&page);
		assert_not_contains(main, "id=\"tpl-deploy\"");
		assert_not_contains(main, "commandfor=\"tpl-deploy\"");
		assert_contains(main, "can look at templates but not deploy");

		let (_, _, body) = post_signals(
			&f.app,
			&format!("/teams/{}/templates/{id}/deploy", f.team),
			Some(&carol),
			&f.deploy_signals(id),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
	}
	assert_eq!(count(&db, "applications").await, 0);
	assert_eq!(count(&db, "stacks").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_cross_origin_deploy_is_refused_and_creates_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, _, _) = crate::testing::send(
		&f.app,
		"POST",
		&format!("/teams/{}/templates/gotify/deploy", f.team),
		Some(&f.cookie),
		Some(&f.deploy_signals("gotify").to_string()),
		Some("http://evil.example"),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "applications").await, 0);
}
