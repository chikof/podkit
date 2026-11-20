//! Tests of the stack pages: create from a pasted compose file, the stack page and its tabs,
//! the actions, and who may do what.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::tests::grid::env_id;
use crate::tests::stacks_ui::{
	job_kinds, newest_stack, service_domain, set_service, set_service_message, set_stack_status,
	stack_compose, stack_name, stack_status, stack_variable,
};
use crate::tests::{
	PgPool, call, count, db_test, local_server_id, project_id, registries_ui, user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, send, ui_login,
	ui_session,
};

const COMPOSE: &str = "services:\n  web:\n    image: nginx:1.27\n    expose: [\"80\"]\n    environment:\n      TOKEN: ${TOKEN}\n  cache:\n    image: redis:7\n";

/// Two refusals in two services, and one at the top.
const BAD_COMPOSE: &str = "services:\n  web:\n    image: nginx\n    build: .\n    devices: [\"/dev/fuse\"]\n  db:\n    image: postgres\n    pid: host\nconfigs:\n  c: {}\n";

struct Fixture {
	app: Router,
	state: crate::state::AppState,
	db: PgPool,
	cookie: String,
	team: i64,
	project: i64,
	environment: i64,
	server: i64,
}

impl Fixture {
	async fn new(db: &PgPool) -> Self {
		let state = crate::tests::state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
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
		Self {
			app,
			state,
			db: db.clone(),
			cookie,
			team,
			project,
			environment: env_id(db, project, "production").await,
			server: local_server_id(db, team).await,
		}
	}

	fn base(&self) -> String {
		format!("/teams/{}/projects/{}/stacks", self.team, self.project)
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	async fn post(&self, uri: &str, signals: &Value) -> String {
		let (s, _, body) = post_signals(&self.app, uri, Some(&self.cookie), signals).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		body
	}

	fn create_signals(&self, name: &str, compose: &str) -> Value {
		json!({
			"name": name,
			"server": self.server.to_string(),
			"compose": compose,
			"envText": "",
			"deploy": false,
			"environment": self.environment.to_string(),
		})
	}

	/// Creates a stack through the page's action and returns its id.
	async fn create(&self, name: &str, compose: &str) -> i64 {
		let body = self
			.post(&self.base(), &self.create_signals(name, compose))
			.await;
		assert_contains(&body, "window.location.assign");
		newest_stack(&self.db).await
	}

	fn stack(&self, id: i64) -> String {
		format!("{}/{id}", self.base())
	}
}

fn redirect_of(body: &str) -> String {
	body.split("assign('")
		.nth(1)
		.and_then(|s| s.split('\'').next())
		.unwrap_or_else(|| panic!("no redirect in {body}"))
		.to_owned()
}

/// Adds Carol with the built-in read-only Member role and signs her in.
async fn read_only_member(f: &Fixture) -> String {
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
	ui_login(&f.app, "carol").await
}

// --- create ---------------------------------------------------------------------------

#[db_test]
async fn the_create_page_takes_a_compose_file_and_offers_to_check_it_first(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, body) = f
		.get(&format!("{}/new?environment={}", f.base(), f.environment))
		.await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	for text in [
		"New compose stack",
		"docker-compose.yml",
		"data-bind:compose",
		"data-bind:server",
		"data-bind=\"envText\"",
		"Check file",
		"Create stack",
		"id=\"compose-check\"",
	] {
		assert_contains(main, text);
	}
	assert_contains(
		main,
		&format!("&quot;environment&quot;:&quot;{}&quot;", f.environment),
	);
	assert_contains(main, &format!("{}/validate", f.base()));
}

#[db_test]
async fn an_invalid_compose_file_shows_every_refused_key_with_its_service_and_path(db: PgPool) {
	let f = Fixture::new(&db).await;
	let signals = f.create_signals("Bad", BAD_COMPOSE);
	let check = f.post(&format!("{}/validate", f.base()), &signals).await;
	for text in [
		"data-state=\"invalid\"",
		"services.web.build",
		"build is not supported, use an image",
		"services.web.devices",
		"device passthrough is not supported",
		"services.db.pid",
		"configs",
	] {
		assert_contains(&check, text);
	}
	assert_contains(&check, ">web<");
	assert_contains(&check, ">db<");
	assert_contains(&check, "whole file");
	assert_contains(&check, " problems");

	// Creating it says the same and stores nothing.
	let created = f.post(&f.base(), &signals).await;
	assert_contains(&created, "services.web.build");
	assert_contains(&created, "services.db.pid");
	assert_not_contains(&created, "window.location.assign");
	assert_eq!(count(&db, "stacks").await, 0);
}

#[db_test]
async fn a_valid_file_passes_the_check_and_names_the_variables_it_lacks(db: PgPool) {
	let f = Fixture::new(&db).await;
	let check = f
		.post(
			&format!("{}/validate", f.base()),
			&f.create_signals("Ok", COMPOSE),
		)
		.await;
	assert_contains(&check, "data-state=\"valid\"");
	assert_contains(&check, "This file can be deployed");
	assert_contains(&check, "TOKEN");
	assert_eq!(count(&db, "stacks").await, 0, "checking stores nothing");

	let mut with_var = f.create_signals("Ok", COMPOSE);
	with_var["envText"] = json!("TOKEN=abc");
	let check = f.post(&format!("{}/validate", f.base()), &with_var).await;
	assert_not_contains(&check, "variables that are not set");

	let broken = f.create_signals("Ok", "services: [");
	let check = f.post(&format!("{}/validate", f.base()), &broken).await;
	assert_contains(&check, "data-state=\"invalid\"");
}

#[db_test]
async fn creating_a_stack_stores_it_and_deploying_opens_the_live_log(db: PgPool) {
	let f = Fixture::new(&db).await;
	let mut signals = f.create_signals("Blog", COMPOSE);
	signals["deploy"] = json!(true);
	signals["envText"] = json!("# secrets\nTOKEN=hunter2-do-not-leak\n");
	let body = f.post(&f.base(), &signals).await;
	let id = newest_stack(&db).await;
	assert_eq!(redirect_of(&body), format!("{}/deployments", f.stack(id)));
	assert_eq!(stack_status(&db, id).await, "deploying");
	assert_eq!(count(&db, "jobs").await, 1, "the first deploy is queued");
	assert_eq!(
		stack_variable(&f.state, id, "TOKEN").await.as_deref(),
		Some("hunter2-do-not-leak")
	);

	// Without the box ticked it only stores.
	let body = f.post(&f.base(), &f.create_signals("Wiki", COMPOSE)).await;
	let wiki = newest_stack(&db).await;
	assert_ne!(wiki, id);
	assert_eq!(redirect_of(&body), f.stack(wiki));
	assert_eq!(count(&db, "jobs").await, 1);
}

#[db_test]
async fn create_says_what_is_wrong_with_the_form_inline(db: PgPool) {
	let f = Fixture::new(&db).await;
	let mut no_name = f.create_signals("", COMPOSE);
	assert_contains(
		&f.post(&f.base(), &no_name).await,
		"name must be 1 to 60 characters",
	);
	no_name["name"] = json!("x");
	no_name["server"] = json!("");
	assert_contains(&f.post(&f.base(), &no_name).await, "Pick a server.");
	let mut bad_env = f.create_signals("x", COMPOSE);
	bad_env["envText"] = json!("fine=1\nnot a pair");
	assert_contains(&f.post(&f.base(), &bad_env).await, "line 2");
	let mut bad_env_id = f.create_signals("x", COMPOSE);
	bad_env_id["environment"] = json!("999999");
	assert_not_contains(&f.post(&f.base(), &bad_env_id).await, "assign");
	assert_eq!(count(&db, "stacks").await, 0);
	f.create("Twice", COMPOSE).await;
	let again = f.post(&f.base(), &f.create_signals("Twice", COMPOSE)).await;
	assert_contains(&again, "already exists");
	assert_eq!(count(&db, "stacks").await, 1);
}

// --- the stack page -------------------------------------------------------------------

#[db_test]
async fn the_stack_page_has_a_header_with_status_and_actions_and_every_tab(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let (s, body) = f.get(&f.stack(id)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "<h1");
	assert_contains(main, "Blog");
	assert_contains(main, "Not deployed");
	assert_contains(main, "Compose file");
	for tab in [
		"Services",
		"Compose",
		"Environment",
		"Domains",
		"Deployments",
		"Logs",
		"Advanced",
	] {
		assert_contains(main, &format!(">{tab}</a>"));
	}
	assert_contains(main, "aria-current=\"page\"");
	assert_contains(main, &format!("@post('{}/deploy')", f.stack(id)));
	assert_contains(main, &format!("@post('{}/stop')", f.stack(id)));
	assert_contains(main, "More actions for Blog");
	// Every service, with its image.
	assert_contains(main, ">web<");
	assert_contains(main, "nginx:1.27");
	assert_contains(main, ">cache<");
	assert_contains(main, "redis:7");
	// The variable the file uses and the stack lacks is called out.
	assert_contains(main, "variables that are not set");
	assert_contains(main, "TOKEN");
}

#[db_test]
async fn the_services_tab_shows_status_health_domain_restart_and_logs(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	set_stack_status(&db, id, "stopped", None).await;
	set_service(&db, id, "web", "running", Some("ctr-web"), Some("healthy")).await;
	set_service(&db, id, "cache", "failed", Some("ctr-cache"), None).await;
	set_service_message(&db, id, "cache", "exited with code 3 <b>").await;
	// Services are listed by name: cache is 0, web is 1.
	f.post(
		&format!("{}/domains/web", f.stack(id)),
		&json!({ "h0": "", "h1": "blog.example.com", "p0": "", "p1": "80" }),
	)
	.await;
	let (_, body) = f.get(&f.stack(id)).await;
	let main = page_main(&body);
	assert_contains(main, "Running");
	assert_contains(main, "healthy");
	assert_contains(main, "no check");
	assert_contains(main, "Failed");
	assert_contains(main, "exited with code 3 &lt;b&gt;");
	assert_not_contains(main, "<b>");
	assert_contains(main, "blog.example.com");
	assert_contains(main, "http://blog.example.com:8088");
	assert_contains(main, &format!("{}/services/web/restart", f.stack(id)));
	assert_contains(main, &format!("{}/logs?service=web", f.stack(id)));
}

#[db_test]
async fn restarting_a_service_queues_a_job_and_opens_the_log(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let body = f
		.post(&format!("{}/services/web/restart", f.stack(id)), &json!({}))
		.await;
	assert_eq!(redirect_of(&body), format!("{}/deployments", f.stack(id)));
	assert_eq!(count(&db, "jobs").await, 1);
	let body = f
		.post(
			&format!("{}/services/nope/restart", f.stack(id)),
			&json!({}),
		)
		.await;
	assert_contains(&body, "_action_error");
	assert_eq!(count(&db, "jobs").await, 1);
}

#[db_test]
async fn deploy_and_stop_queue_jobs_and_a_busy_stack_polls_its_status(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f
		.create("Blog", "services:\n  web:\n    image: nginx\n")
		.await;
	let (_, idle) = f.get(&f.stack(id)).await;
	assert_not_contains(&idle, "data-on-interval");

	let body = f.post(&format!("{}/deploy", f.stack(id)), &json!({})).await;
	assert_eq!(redirect_of(&body), format!("{}/deployments", f.stack(id)));
	assert_eq!(stack_status(&db, id).await, "deploying");
	let (_, busy) = f.get(&f.stack(id)).await;
	assert_contains(&busy, "data-on-interval__duration.3s");
	assert_contains(&busy, &format!("{}/status?tab=services", f.stack(id)));
	let (s, poll) = f.get(&format!("{}/status?tab=services", f.stack(id))).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&poll, "id=\"stack-header\"");
	assert_contains(&poll, "id=\"stack-services\"");

	set_stack_status(&db, id, "stopped", None).await;
	let stop = f.post(&format!("{}/stop", f.stack(id)), &json!({})).await;
	assert_contains(&stop, "window.location.reload()");
	let (_, done) = f.get(&f.stack(id)).await;
	assert_not_contains(&done, "data-on-interval");
	assert_eq!(count(&db, "jobs").await, 2);
}

#[db_test]
async fn a_failed_stack_says_why_and_the_text_is_escaped(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("<script>alert(1)</script>", COMPOSE).await;
	set_stack_status(&db, id, "failed", Some("web: <img src=x onerror=alert(1)>")).await;
	for tab in [
		"",
		"/compose",
		"/env",
		"/domains",
		"/deployments",
		"/logs",
		"/advanced",
	] {
		let (s, page) = f.get(&format!("{}{tab}", f.stack(id))).await;
		assert_eq!(s, StatusCode::OK, "{tab}");
		assert_not_contains(&page, "<script>alert(1)");
		assert_not_contains(&page, "<img src=x");
	}
	let (_, page) = f.get(&f.stack(id)).await;
	assert_contains(&page, "Failed");
	assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_contains(&page, "web: &lt;img src=x onerror=alert(1)&gt;");
}

// --- compose tab ----------------------------------------------------------------------

#[db_test]
async fn the_compose_tab_shows_the_file_and_a_save_is_checked_before_it_is_stored(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let url = format!("{}/compose", f.stack(id));
	let (s, page) = f.get(&url).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "&quot;compose&quot;:&quot;services:");
	assert_contains(&page, "data-bind:compose");
	assert_contains(&page, "Save compose file");

	// A change podkit could not run is refused with every key and nothing changes.
	let refused = f.post(&url, &json!({ "compose": BAD_COMPOSE })).await;
	assert_contains(&refused, "services.web.build");
	assert_contains(&refused, "services.db.pid");
	assert_eq!(stack_compose(&db, id).await, COMPOSE);

	let fine = "services:\n  web:\n    image: nginx:1.28\n";
	let saved = f.post(&url, &json!({ "compose": fine })).await;
	assert_contains(&saved, "Saved. Deploy the stack to apply the change.");
	assert_eq!(stack_compose(&db, id).await, fine);
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "nginx:1.28");
	assert_not_contains(&page, "redis:7");
}

// --- environment tab ------------------------------------------------------------------

#[db_test]
async fn variables_are_write_only_names_are_listed_and_values_never_come_back(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let url = format!("{}/env", f.stack(id));
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "Missing variables");
	assert_contains(&page, "No variables set.");

	let secret = "hunter2-do-not-leak";
	let set = f
		.post(&url, &json!({ "envKey": "TOKEN", "envValue": secret }))
		.await;
	assert_contains(&set, "window.location.reload()");
	assert_not_contains(&set, secret);
	assert_eq!(
		stack_variable(&f.state, id, "TOKEN").await.as_deref(),
		Some(secret)
	);
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "data-env-key=\"TOKEN\"");
	assert_contains(&page, "value hidden");
	assert_not_contains(&page, secret);
	assert_not_contains(&page, "Missing variables");
	for tab in [
		"",
		"/compose",
		"/domains",
		"/deployments",
		"/logs",
		"/advanced",
	] {
		let (_, other) = f.get(&format!("{}{tab}", f.stack(id))).await;
		assert_not_contains(&other, secret);
	}

	let bad = f
		.post(&url, &json!({ "envKey": "", "envValue": "x" }))
		.await;
	assert_contains(&bad, "_error");
	assert_not_contains(&bad, "reload");
	let removed = f
		.post(&format!("{url}/unset"), &json!({ "unsetKey": "TOKEN" }))
		.await;
	assert_contains(&removed, "window.location.reload()");
	assert_eq!(stack_variable(&f.state, id, "TOKEN").await, None);
}

// --- domains tab ----------------------------------------------------------------------

#[db_test]
async fn a_hostname_is_routed_to_a_service_and_can_be_removed(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let url = format!("{}/domains", f.stack(id));
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "data-service=\"web\"");
	assert_contains(&page, "data-service=\"cache\"");
	assert_contains(&page, "It starts answering after the next deploy");

	// Services are listed by name: cache is 0, web is 1.
	let bad = f
		.post(
			&format!("{url}/web"),
			&json!({ "h0": "", "h1": "not a host!", "p1": "80" }),
		)
		.await;
	assert_contains(&bad, "That hostname isn't valid.");
	let bad_port = f
		.post(
			&format!("{url}/web"),
			&json!({ "h1": "blog.example.com", "p1": "eighty" }),
		)
		.await;
	assert_contains(&bad_port, "The port must be a number.");
	assert_eq!(service_domain(&db, id, "web").await, None);

	let ok = f
		.post(
			&format!("{url}/web"),
			&json!({ "h0": "", "h1": "Blog.Example.com", "p1": "80" }),
		)
		.await;
	assert_contains(&ok, "window.location.reload()");
	assert_eq!(
		service_domain(&db, id, "web").await.as_deref(),
		Some("blog.example.com")
	);
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "blog.example.com");

	// Another stack cannot take it.
	let other = f.create("Wiki", COMPOSE).await;
	assert_ne!(other, id);
	let taken = f
		.post(
			&format!("{}/domains/web", f.stack(other)),
			&json!({ "h1": "blog.example.com", "p1": "80" }),
		)
		.await;
	assert_not_contains(&taken, "reload");

	f.post(&format!("{url}/web/clear"), &json!({})).await;
	assert_eq!(service_domain(&db, id, "web").await, None);
	let missing = f
		.post(&format!("{url}/nope"), &json!({ "h0": "a.example.com" }))
		.await;
	assert_contains(&missing, "That no longer exists.");
}

// --- deployments and logs -------------------------------------------------------------

#[db_test]
async fn the_deployments_tab_shows_the_latest_job_with_a_live_log(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f
		.create("Blog", "services:\n  web:\n    image: nginx\n")
		.await;
	let url = format!("{}/deployments", f.stack(id));
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "Not deployed yet");
	assert_not_contains(&page, "id=\"deploy-log\"");
	// With no job the log stream ends at once.
	let (s, log) = f.get(&format!("{url}/log")).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&log, "_live");

	f.post(&format!("{}/deploy", f.stack(id)), &json!({})).await;
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "Latest deployment");
	assert_contains(&page, "Queued");
	assert_contains(&page, "id=\"deploy-log\"");
	assert_contains(&page, &format!("{url}/log"));
	assert_contains(&page, "id=\"stack-job-status\"");
}

#[db_test]
async fn the_logs_tab_picks_a_running_service_and_says_so_when_none_runs(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let url = format!("{}/logs", f.stack(id));
	let (_, page) = f.get(&url).await;
	assert_contains(&page, "No service is running.");

	set_service(&db, id, "web", "running", Some("ctr-web"), None).await;
	set_service(&db, id, "cache", "running", Some("ctr-cache"), None).await;
	let (_, page) = f.get(&format!("{url}?service=cache&tail=1000")).await;
	assert_contains(&page, "name=\"service\"");
	assert_contains(&page, "<option value=\"cache\" selected>");
	assert_contains(&page, "<option value=\"web\">");
	assert_contains(
		&page,
		&format!("{url}/stream?service=cache&amp;tail=1000&amp;follow=1"),
	);
	assert_contains(&page, "id=\"stack-log\"");

	// An unknown service falls back to a running one.
	let (_, page) = f.get(&format!("{url}?service=%3Cb%3E")).await;
	assert_not_contains(&page, "<b>");
	assert_contains(&page, "/logs/stream?service=");
}

// --- advanced -------------------------------------------------------------------------

#[db_test]
async fn removing_needs_the_name_typed_and_can_purge_volumes(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let (_, page) = f.get(&format!("{}/advanced", f.stack(id))).await;
	assert_contains(&page, "Remove this stack");
	assert_contains(&page, "Also delete its volumes");
	assert_contains(
		&page,
		"$confirm !== &#39;Blog&#39;".replace("&#39;", "'").as_str(),
	);

	let url = format!("{}/remove", f.stack(id));
	let wrong = f
		.post(&url, &json!({ "confirm": "blog", "purge": true }))
		.await;
	assert_contains(&wrong, "Type the stack's name exactly");
	assert_eq!(stack_status(&db, id).await, "pending");
	assert_eq!(count(&db, "jobs").await, 0);

	let gone = f
		.post(&url, &json!({ "confirm": "Blog", "purge": true }))
		.await;
	assert_eq!(
		redirect_of(&gone),
		format!(
			"/teams/{}/projects/{}?environment={}",
			f.team, f.project, f.environment
		)
	);
	assert_eq!(stack_status(&db, id).await, "deleting");
	let kinds = job_kinds(&db).await;
	assert_eq!(kinds.len(), 1);
	assert!(kinds[0].contains("purge"), "{kinds:?}");
	assert_eq!(stack_name(&db, id).await, "Blog");

	// Without the box ticked the volumes stay.
	let keep = f.create("Wiki", COMPOSE).await;
	f.post(
		&format!("{}/remove", f.stack(keep)),
		&json!({ "confirm": "Wiki", "purge": false }),
	)
	.await;
	let kinds = job_kinds(&db).await;
	assert_eq!(kinds.len(), 2);
	assert!(!kinds[1].contains("purge"), "{kinds:?}");
}

// --- the environment view -------------------------------------------------------------

#[db_test]
async fn stacks_are_cards_in_their_environment_with_a_template_logo_and_a_filter(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.create("Blog", COMPOSE).await;
	// A template stack carries the template's own logo.
	f.post(
		&format!("/teams/{}/templates/umami/deploy", f.team),
		&json!({
			"project": f.project.to_string(),
			"environment": f.environment.to_string(),
			"server": f.server.to_string(),
			"name": "",
			"v0": "",
		}),
	)
	.await;
	let (_, page) = f
		.get(&format!("/teams/{}/projects/{}", f.team, f.project))
		.await;
	let main = page_main(&page);
	assert_eq!(main.matches("data-kind=\"stack\"").count(), 2);
	assert_contains(main, ">Stack<");
	assert_contains(main, "Not deployed");
	assert_contains(main, "Deploying");
	assert_contains(main, "2 services");
	assert_contains(main, "data-logo=\"image\"");
	assert_contains(main, "data-logo=\"docker\"");
	assert_contains(main, "value=\"stack\"");
	let (_, only) = f
		.get(&format!(
			"/teams/{}/projects/{}?type=stack",
			f.team, f.project
		))
		.await;
	assert_eq!(page_main(&only).matches("data-kind=").count(), 2);
	let (_, none) = f
		.get(&format!(
			"/teams/{}/projects/{}?type=database",
			f.team, f.project
		))
		.await;
	assert_not_contains(page_main(&none), "data-kind=\"stack\"");
}

// --- who may do what ------------------------------------------------------------------

#[db_test]
async fn another_teams_stacks_are_the_404_page_and_leak_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let (_, mallory_team) = user_with_team(&f.app, "mallory").await;
	let mallory = ui_login(&f.app, "mallory").await;
	let (s, _, _) = post_signals(
		&f.app,
		&format!("/teams/{mallory_team}/projects"),
		Some(&mallory),
		&json!({ "name": "Mine", "slug": "mine" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let mine = project_id(&db, "mine").await;

	for tail in [
		"",
		"/compose",
		"/env",
		"/domains",
		"/deployments",
		"/deployments/log",
		"/logs",
		"/logs/stream?service=web",
		"/advanced",
		"/status",
	] {
		// Through the other team, through the other team's project, and through ours.
		for base in [
			f.stack(id),
			format!("/teams/{mallory_team}/projects/{}/stacks/{id}", f.project),
			format!("/teams/{mallory_team}/projects/{mine}/stacks/{id}"),
		] {
			let (s, _, body) = get_html(&f.app, &format!("{base}{tail}"), Some(&mallory)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{base}{tail}");
			assert_not_contains(&body, "Blog");
			assert_not_contains(&body, "nginx");
		}
	}
	let (s, _, _) = get_html(&f.app, &format!("{}/new", f.base()), Some(&mallory)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	for tail in [
		"/deploy",
		"/stop",
		"/remove",
		"/compose",
		"/env",
		"/services/web/restart",
	] {
		for base in [
			f.stack(id),
			format!("/teams/{mallory_team}/projects/{mine}/stacks/{id}"),
		] {
			let (_, _, body) = post_signals(
				&f.app,
				&format!("{base}{tail}"),
				Some(&mallory),
				&json!({ "confirm": "Blog", "compose": "services: {}", "envKey": "A", "envValue": "b" }),
			)
			.await;
			assert_not_contains(&body, "window.location");
		}
	}
	assert_eq!(stack_status(&db, id).await, "pending");
	assert_eq!(stack_compose(&db, id).await, COMPOSE);
	assert_eq!(count(&db, "jobs").await, 0);
	// Creating in her own project on our server is refused too.
	let (_, _, body) = post_signals(
		&f.app,
		&format!("/teams/{mallory_team}/projects/{mine}/stacks"),
		Some(&mallory),
		&f.create_signals("Sneaky", COMPOSE),
	)
	.await;
	assert_not_contains(&body, "window.location");
	assert_eq!(count(&db, "stacks").await, 1);
}

#[db_test]
async fn a_read_only_member_sees_the_stack_without_actions_and_forged_posts_are_refused(
	db: PgPool,
) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	f.post(
		&format!("{}/env", f.stack(id)),
		&json!({ "envKey": "TOKEN", "envValue": "hunter2-do-not-leak" }),
	)
	.await;
	let carol = read_only_member(&f).await;
	let stack = f.stack(id);

	for tail in ["", "/compose", "/env", "/domains", "/deployments", "/logs"] {
		let (s, _, page) = get_html(&f.app, &format!("{stack}{tail}"), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK, "{tail}");
		for control in [
			">Deploy<",
			">Stop<",
			"/deploy')",
			"/stop')",
			"/restart')",
			"Save compose file",
			"Save variable",
			"Set a variable",
			">Remove<",
			"Remove stack",
			">Advanced<",
			"hunter2",
		] {
			assert_not_contains(page_main(&page), control);
		}
	}
	let (s, _, _) = get_html(&f.app, &format!("{stack}/advanced"), Some(&carol)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);

	let before_jobs = count(&db, "jobs").await;
	for (tail, signals) in [
		("/deploy", json!({})),
		("/stop", json!({})),
		("/services/web/restart", json!({})),
		(
			"/compose",
			json!({ "compose": "services:\n  web:\n    image: evil\n" }),
		),
		("/env", json!({ "envKey": "X", "envValue": "y" })),
		("/env/unset", json!({ "unsetKey": "TOKEN" })),
		(
			"/domains/web",
			json!({ "h1": "evil.example.com", "p1": "80" }),
		),
		("/domains/web/clear", json!({})),
		("/remove", json!({ "confirm": "Blog", "purge": true })),
	] {
		let (_, _, body) =
			post_signals(&f.app, &format!("{stack}{tail}"), Some(&carol), &signals).await;
		assert_contains(&body, "You don't have permission to do that.");
		assert_not_contains(&body, "window.location");
	}
	for (uri, signals) in [
		(
			format!("{}/validate", f.base()),
			f.create_signals("x", COMPOSE),
		),
		(f.base(), f.create_signals("Other", COMPOSE)),
	] {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
		assert_contains(&body, "You don't have permission to do that.");
	}

	assert_eq!(count(&db, "jobs").await, before_jobs);
	assert_eq!(count(&db, "stacks").await, 1);
	assert_eq!(stack_compose(&db, id).await, COMPOSE);
	assert_eq!(stack_status(&db, id).await, "pending");
	assert_eq!(
		stack_variable(&f.state, id, "TOKEN").await.as_deref(),
		Some("hunter2-do-not-leak")
	);
}

#[db_test]
async fn a_cross_origin_post_to_a_stack_is_refused_and_changes_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let evil = "http://evil.example";
	for (tail, body) in [
		("/deploy", json!({})),
		("/stop", json!({})),
		("/remove", json!({ "confirm": "Blog" })),
		("/env", json!({ "envKey": "A", "envValue": "b" })),
	] {
		let (s, _, _) = send(
			&f.app,
			"POST",
			&format!("{}{tail}", f.stack(id)),
			Some(&f.cookie),
			Some(&body.to_string()),
			Some(evil),
		)
		.await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{tail}");
	}
	let (s, _, _) = send(
		&f.app,
		"POST",
		&f.base(),
		Some(&f.cookie),
		Some(&f.create_signals("Evil", COMPOSE).to_string()),
		Some(evil),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "stacks").await, 1);
	assert_eq!(stack_status(&db, id).await, "pending");
}

#[db_test]
async fn an_unauthenticated_stack_page_and_action_are_sent_to_sign_in(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create("Blog", COMPOSE).await;
	let (s, headers, _) = get_html(&f.app, &f.stack(id), None).await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert!(headers["location"].to_str().unwrap().starts_with("/login"));
	let (_, _, body) =
		post_signals(&f.app, &format!("{}/deploy", f.stack(id)), None, &json!({})).await;
	assert_contains(&body, "/login");
}

/// A stack's pages sit in the environment it lives in, not the project's first one: the
/// breadcrumb names it.
#[db_test]
async fn a_stack_outside_the_first_environment_is_named_in_the_breadcrumb(db: PgPool) {
	let f = Fixture::new(&db).await;
	f.post(
		&format!("/teams/{}/projects/{}/environments", f.team, f.project),
		&json!({ "newName": "Staging", "newSlug": "staging" }),
	)
	.await;
	let staging = env_id(&db, f.project, "staging").await;
	let mut signals = f.create_signals("Blog", COMPOSE);
	signals["environment"] = json!(staging.to_string());
	f.post(&f.base(), &signals).await;
	let id = newest_stack(&db).await;
	let (s, body) = f.get(&f.stack(id)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "class=\"ui-crumb-link\">Staging</a>");
	assert_not_contains(&body, "class=\"ui-crumb-link\">production</a>");
}

#[db_test]
async fn a_stack_can_pick_a_saved_registry_and_a_foreign_one_is_refused(db: PgPool) {
	let f = Fixture::new(&db).await;
	let new = format!("{}/new", f.base());

	// No saved registries: the picker says so and points at where to add one.
	let (_, body) = f.get(&new).await;
	assert_contains(page_main(&body), "No saved registries yet.");

	let (s, _, _) = post_signals(
		&f.app,
		&format!("/teams/{}/registries", f.team),
		Some(&f.cookie),
		&json!({ "name": "GitHub <b>", "url": "ghcr.io", "username": "robot", "password": "pw-secret" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let registry = registries_ui::registry_id(&db).await;
	let (_, body) = f.get(&new).await;
	let main = page_main(&body);
	assert_contains(main, "GitHub &lt;b&gt; · ghcr.io");
	assert_not_contains(main, "pw-secret");
	assert_contains(main, "data-bind:registry_id");

	let mut signals = f.create_signals("db", COMPOSE);
	signals["registry_id"] = json!(registry.to_string());
	f.post(&f.base(), &signals).await;
	let used = registries_ui::stack_registry(&db).await;
	assert_eq!(used, Some(registry));

	// An id that is not the team's, or not a number, makes nothing.
	for bad in ["999999", "x"] {
		let mut signals = f.create_signals("other", COMPOSE);
		signals["registry_id"] = json!(bad);
		let (s, _, _) = post_signals(&f.app, &f.base(), Some(&f.cookie), &signals).await;
		assert_eq!(s, StatusCode::OK);
	}
	assert_eq!(count(&db, "stacks").await, 1);
}
