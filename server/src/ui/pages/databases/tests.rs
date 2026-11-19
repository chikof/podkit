//! Tests of the database pages. Most were the tests of the pre-redesign pages: each keeps
//! its name, with its assertions moved to where the redesign put the thing it checks (the
//! password to the Connection tab, the schedule to the Backups tab, delete to the dialog).

use axum::Router;
use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use crate::tests::dbs::{
	add_backup_target, add_job, backup_settings, database_environment, database_id, delete_jobs,
	job_kinds, local_server, set_jobs, set_state, status_of,
};
use crate::tests::grid::env_id;
use crate::tests::{PgPool, app, call, count, db_test, user_with_team};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};

struct Fixture {
	app: Router,
	token: String,
	cookie: String,
	team: i64,
	project: i64,
	server: i64,
}

impl Fixture {
	async fn new(db: &PgPool) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let cookie = ui_login(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "Shop", "slug": "shop" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s}");
		let server = local_server(db, team).await;
		Self {
			app,
			token,
			cookie,
			team,
			project: project["id"].as_i64().unwrap(),
			server,
		}
	}

	fn base(&self) -> String {
		format!("/teams/{}/projects/{}/databases", self.team, self.project)
	}

	fn url(&self, id: i64) -> String {
		format!("{}/{id}", self.base())
	}

	#[allow(clippy::needless_pass_by_value)]
	fn form(&self, name: &str, extra: Value) -> Value {
		let mut form = json!({
			"name": name, "engine": "postgres", "version": "",
			"server_id": self.server.to_string(),
			"public_port": "", "memory_limit_mb": "", "cpu_limit": "",
		});
		for (k, v) in extra.as_object().unwrap() {
			form[k] = v.clone();
		}
		form
	}

	/// Creates a database through the UI and returns its id.
	async fn create(&self, db: &PgPool, name: &str, extra: Value) -> i64 {
		let (s, _, body) = post_signals(
			&self.app,
			&self.base(),
			Some(&self.cookie),
			&self.form(name, extra),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		let id = database_id(db, name).await;
		assert_contains(
			&body,
			&format!("window.location.assign('{}')", self.url(id)),
		);
		id
	}

	async fn get(&self, uri: &str) -> (StatusCode, String) {
		let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
		(s, body)
	}

	async fn post(&self, uri: &str, signals: &Value) -> String {
		post_signals(&self.app, uri, Some(&self.cookie), signals)
			.await
			.2
	}

	async fn password(&self, id: i64) -> String {
		let (s, db) = call(
			&self.app,
			"GET",
			&format!(
				"/api/v1/teams/{}/projects/{}/databases/{id}",
				self.team, self.project
			),
			Some(&self.token),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		db["connection"]["password"].as_str().unwrap().to_owned()
	}

	/// A member with the built-in read-only role, signed in.
	async fn member(&self, name: &str) -> String {
		user_with_team(&self.app, name).await;
		let (s, _) = call(
			&self.app,
			"POST",
			&format!("/api/v1/teams/{}/members", self.team),
			Some(&self.token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
		ui_login(&self.app, name).await
	}
}

#[allow(clippy::needless_pass_by_value)]
fn saved(extra: Value) -> Value {
	let mut form = json!({
		"public_port": "", "memory_limit_mb": "", "cpu_limit": "",
		"backup_target_id": "", "backup_schedule": "", "backup_keep": "7",
	});
	for (k, v) in extra.as_object().unwrap() {
		form[k] = v.clone();
	}
	form
}

#[db_test]
async fn the_create_form_offers_engines_defaults_and_active_servers(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, body) = f.get(&format!("{}/new", f.base())).await;
	assert_eq!(s, StatusCode::OK);
	for engine in ["PostgreSQL", "MySQL", "MongoDB", "Redis"] {
		assert_contains(&body, engine);
	}
	// A logo card per engine, as native radios (one group, one choice).
	for mark in ["postgresql", "mysql", "mongodb", "redis"] {
		assert_contains(&body, &format!("data-logo=\"{mark}\""));
	}
	assert_eq!(body.matches("type=\"radio\" name=\"engine\"").count(), 4);
	assert_contains(&body, "<strong>18</strong>");
	assert_contains(&body, "<strong>8</strong>");
	assert_contains(&body, "Engine default");
	assert_contains(&body, &format!("value=\"{}\"", f.server));
	assert_contains(&body, "Create database");
	assert_contains(&body, "Public port");
	assert_contains(&body, "Memory limit in MB");
}

#[db_test]
async fn the_create_form_preselects_the_engine_and_carries_the_environment(db: PgPool) {
	let f = Fixture::new(&db).await;
	let production = env_id(&db, f.project, "production").await;
	let uri = format!("{}/new?environment={production}&engine=mongodb", f.base());
	let (_, body) = f.get(&uri).await;
	assert_contains(&body, "&quot;engine&quot;:&quot;mongodb&quot;");
	assert_contains(
		&body,
		&format!("&quot;environment&quot;:&quot;{production}&quot;"),
	);
	assert_contains(&body, "value=\"mongodb\" checked");
	// Nonsense falls back to the default engine.
	let (_, body) = f.get(&format!("{}/new?engine=oracle", f.base())).await;
	assert_contains(&body, "&quot;engine&quot;:&quot;postgres&quot;");
}

#[db_test]
async fn a_database_is_created_in_the_chosen_environment(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, _, body) = post_signals(
		&f.app,
		&format!("/teams/{}/projects/{}/environments", f.team, f.project),
		Some(&f.cookie),
		&json!({ "newName": "Staging", "newSlug": "staging" }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "/staging");
	let staging = env_id(&db, f.project, "staging").await;
	let production = env_id(&db, f.project, "production").await;

	let id = f
		.create(&db, "staged", json!({ "environment": staging.to_string() }))
		.await;
	assert_eq!(database_environment(&db, id).await, staging);
	let id = f.create(&db, "plain", json!({})).await;
	assert_eq!(database_environment(&db, id).await, production);

	let body = f
		.post(
			&f.base(),
			&f.form("elsewhere", json!({ "environment": "999999" })),
		)
		.await;
	assert_not_contains(&body, "window.location");
}

#[db_test]
async fn creating_queues_the_provision_job_and_opens_the_database(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f
		.create(
			&db,
			"orders",
			json!({ "memory_limit_mb": "512", "cpu_limit": "1.5" }),
		)
		.await;
	assert_eq!(job_kinds(&db, id).await, ["database_provision"]);
	assert_eq!(status_of(&db, id).await, "pending");

	let (s, page) = f.get(&f.url(id)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "orders");
	assert_contains(&page, "PostgreSQL 18");
	assert_contains(&page, "Pending");
	// The provision job is queued, so the header polls and the page shows its log.
	assert_contains(&page, "data-on-interval__duration.3s");
	assert_contains(&page, &format!("{}/header", f.url(id)));
	assert_contains(&page, &format!("{}/job-log", f.url(id)));
	// The tabs: one current page.
	for tab in ["General", "Connection", "Backups", "Logs", "Advanced"] {
		assert_contains(&page, tab);
	}
	assert_eq!(page_main(&page).matches("aria-current=\"page\"").count(), 1);
}

#[db_test]
async fn bad_input_is_an_inline_message_and_creates_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	for (extra, message) in [
		(json!({ "name": "" }), "name must be 1 to 60 characters"),
		(json!({ "version": "7" }), "unsupported version 7"),
		(
			json!({ "public_port": "80" }),
			"public_port must be between",
		),
		(
			json!({ "public_port": "abc" }),
			"Public port must be a number.",
		),
		(
			json!({ "memory_limit_mb": "lots" }),
			"Memory limit must be a number.",
		),
		(json!({ "server_id": "" }), "Pick a server."),
		(json!({ "engine": "oracle" }), "Pick an engine."),
	] {
		let body = f.post(&f.base(), &f.form("db", extra)).await;
		assert_contains(&body, message);
		assert_not_contains(&body, "window.location");
	}
	assert_eq!(count(&db, "databases").await, 0);
	assert_eq!(count(&db, "jobs").await, 0);
}

#[db_test]
async fn the_password_is_only_in_the_reveal_response(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f
		.create(&db, "orders", json!({ "public_port": "15432" }))
		.await;
	let password = f.password(id).await;
	let tab = format!("{}/connection", f.url(id));

	for uri in [f.url(id), tab.clone()] {
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_not_contains(&page, &password);
		assert_not_contains(&page, "postgres://");
	}
	let (_, page) = f.get(&tab).await;
	assert_contains(&page, &format!("podkit-db-{id}"));
	assert_contains(&page, "5432");
	assert_contains(&page, "podkit");
	assert_contains(&page, ":15432");
	assert_contains(&page, "Reveal password and URLs");
	assert_contains(&page, "<span>User</span>");
	assert_contains(&page, "id=\"db-secret\"");

	let (s, headers, body) = post_signals(
		&f.app,
		&format!("{}/reveal", f.url(id)),
		Some(&f.cookie),
		&json!({}),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(headers[header::CACHE_CONTROL], "no-store");
	assert_contains(&body, &password);
	assert_contains(
		&body,
		&format!("postgres://podkit:{password}@podkit-db-{id}:5432/podkit"),
	);
	assert_contains(&body, ":15432/podkit");
	assert_contains(&body, "Copy");
	assert_contains(&body, "db-secret");
	assert_contains(&body, "Hide");

	// Hide puts the bullets back, and a page loaded after the reveal is as clean as before.
	let (_, _, hidden) = get_html(&f.app, &format!("{tab}/hide"), Some(&f.cookie)).await;
	assert_contains(&hidden, "db-secret");
	assert_contains(&hidden, "Reveal password and URLs");
	assert_not_contains(&hidden, &password);
	let (_, again) = f.get(&tab).await;
	assert_not_contains(&again, &password);
}

#[db_test]
async fn redis_has_no_user_or_database_to_show(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "cache", json!({ "engine": "redis" })).await;
	let (_, general) = f.get(&f.url(id)).await;
	assert_contains(&general, "Redis 8");
	let (_, page) = f.get(&format!("{}/connection", f.url(id))).await;
	assert_contains(&page, "6379");
	assert_not_contains(&page, "<span>User</span>");
	assert_not_contains(&page, "<span>Database</span>");
}

#[db_test]
async fn a_read_only_member_sees_no_secrets_or_actions(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let password = f.password(id).await;
	set_state(&db, id, "running", Some("c1")).await;
	set_jobs(&db, id, "succeeded").await;
	let carol = f.member("carol").await;

	let (s, _, page) = get_html(&f.app, &f.url(id), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "orders");
	for hidden in [
		"Reveal password",
		"Connection",
		"Restart",
		"Save changes",
		"Advanced",
		"delete-dialog",
	] {
		assert_not_contains(page_main(&page), hidden);
	}
	let (s, _, page) = get_html(&f.app, &format!("{}/logs", f.url(id)), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Container log");
	let (s, _, page) = get_html(&f.app, &format!("{}/backups", f.url(id)), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "No backups yet.");

	for uri in [
		format!("{}/new", f.base()),
		format!("{}/connection", f.url(id)),
		format!("{}/advanced", f.url(id)),
	] {
		let (s, _, page) = get_html(&f.app, &uri, Some(&carol)).await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
		assert_contains(&page, "permission");
	}

	let jobs_before = job_kinds(&db, id).await;
	for (uri, signals) in [
		(f.base(), f.form("forged", json!({}))),
		(format!("{}/reveal", f.url(id)), json!({})),
		(format!("{}/start", f.url(id)), json!({})),
		(format!("{}/stop", f.url(id)), json!({})),
		(format!("{}/restart", f.url(id)), json!({})),
		(
			format!("{}/settings", f.url(id)),
			saved(json!({ "public_port": "15432" })),
		),
		(
			format!("{}/schedule", f.url(id)),
			saved(json!({ "backup_schedule": "0 3 * * *" })),
		),
		(
			format!("{}/delete", f.url(id)),
			json!({ "confirm": "orders", "purge": true }),
		),
	] {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
		assert_contains(&body, "You don't have permission to do that.");
		assert_not_contains(&body, &password);
		assert_not_contains(&body, "window.location");
	}
	assert_eq!(count(&db, "databases").await, 1);
	assert_eq!(job_kinds(&db, id).await, jobs_before);
	assert_eq!(status_of(&db, id).await, "running");
	assert_eq!(backup_settings(&db, id).await, (None, None, 7, false));
}

#[db_test]
async fn the_header_offers_the_actions_that_can_work(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	set_jobs(&db, id, "succeeded").await;
	set_state(&db, id, "running", Some("c1")).await;
	let (_, page) = f.get(&f.url(id)).await;
	for text in [
		"Start",
		"Restart",
		"Stop",
		"Delete",
		"Actions for orders",
		"db-menu",
	] {
		assert_contains(&page, text);
	}
	// The delete dialog asks for the name and offers the purge as its own choice.
	assert_contains(&page, "id=\"delete-dialog\"");
	assert_contains(
		&page,
		"Type <strong class=\"font-mono break-all\">orders</strong>",
	);
	assert_contains(&page, "Also delete the data");
}

#[db_test]
async fn lifecycle_buttons_queue_the_matching_job_and_reload(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;

	// While the provision job is queued nothing else can be started.
	let (_, page) = f.get(&f.url(id)).await;
	assert_contains(&page, "disabled");

	set_jobs(&db, id, "succeeded").await;
	set_state(&db, id, "stopped", None).await;
	let (_, page) = f.get(&f.url(id)).await;
	assert_not_contains(&page, "data-on-interval");
	assert_contains(&page, "Stopped");

	let body = f.post(&format!("{}/start", f.url(id)), &json!({})).await;
	assert_contains(&body, &format!("window.location.assign('{}')", f.url(id)));
	set_jobs(&db, id, "succeeded").await;
	f.post(&format!("{}/stop", f.url(id)), &json!({})).await;
	set_jobs(&db, id, "succeeded").await;
	f.post(&format!("{}/restart", f.url(id)), &json!({})).await;
	assert_eq!(
		job_kinds(&db, id).await,
		[
			"database_provision",
			"database_start",
			"database_stop",
			"database_provision"
		]
	);

	// The header shows the badge and polls while a job is queued.
	let (_, _, body) = get_html(&f.app, &format!("{}/header", f.url(id)), Some(&f.cookie)).await;
	assert_contains(&body, "datastar-patch-elements");
	assert_contains(&body, "db-header");
	assert_contains(&body, "data-on-interval__duration.3s");
	set_jobs(&db, id, "succeeded").await;
	set_state(&db, id, "running", Some("c1")).await;
	let (_, _, body) = get_html(&f.app, &format!("{}/header", f.url(id)), Some(&f.cookie)).await;
	assert_contains(&body, "Running");
	assert_not_contains(&body, "data-on-interval");
}

#[db_test]
async fn a_schedule_is_checked_inline_with_the_next_three_runs(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let preview = format!("{}/schedule", f.url(id));

	let body = f
		.post(&preview, &saved(json!({ "backup_schedule": "0 3 * * *" })))
		.await;
	assert_contains(&body, "schedule-hint");
	assert_contains(&body, "Next runs");
	assert_eq!(body.matches(":00 UTC</li>").count(), 3, "{body}");
	assert_not_contains(&body, "Not a valid schedule");

	let body = f
		.post(&preview, &saved(json!({ "backup_schedule": "every day" })))
		.await;
	assert_contains(&body, "Not a valid schedule");
	assert_not_contains(&body, "Next runs");

	let body = f
		.post(&preview, &saved(json!({ "backup_schedule": "  " })))
		.await;
	assert_contains(&body, "No schedule");
}

#[db_test]
async fn saving_an_invalid_cron_changes_nothing_and_a_valid_one_is_stored(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let target = add_backup_target(&db, 8001, f.team, "offsite").await;
	let save = format!("{}/settings", f.url(id));

	let body = f
		.post(
			&save,
			&saved(
				json!({ "backup_schedule": "61 * * * *", "backup_target_id": target.to_string() }),
			),
		)
		.await;
	assert_contains(&body, "Not a valid schedule");
	assert_not_contains(&body, "Settings saved");
	assert_eq!(backup_settings(&db, id).await, (None, None, 7, false));

	let body = f
		.post(
			&save,
			&saved(json!({
				"backup_schedule": "0 3 * * *", "backup_target_id": target.to_string(),
				"backup_keep": "5",
			})),
		)
		.await;
	assert_contains(&body, "Settings saved");
	assert_contains(&body, "Next runs");
	assert_eq!(
		backup_settings(&db, id).await,
		(Some(target), Some("0 3 * * *".to_owned()), 5, true)
	);
	// Backup settings alone do not recreate the container.
	assert_eq!(job_kinds(&db, id).await, ["database_provision"]);

	let body = f.post(&save, &saved(json!({ "backup_keep": "0" }))).await;
	assert_contains(&body, "backup_keep must be between 1 and 365");
	let body = f.post(&save, &saved(json!({ "backup_keep": "x" }))).await;
	assert_contains(&body, "Backups to keep must be a number.");

	let (_, page) = f.get(&format!("{}/backups", f.url(id))).await;
	assert_contains(&page, "offsite");
	assert_contains(&page, "0 3 * * *");
	assert_contains(&page, "Next runs");
}

#[db_test]
async fn changing_the_port_or_a_limit_recreates_the_container(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let save = format!("{}/settings", f.url(id));

	let body = f
		.post(&save, &saved(json!({ "public_port": "15433" })))
		.await;
	assert_contains(&body, &format!("window.location.assign('{}')", f.url(id)));
	assert_eq!(
		job_kinds(&db, id).await,
		["database_provision", "database_provision"]
	);

	let body = f.post(&save, &saved(json!({ "public_port": "80" }))).await;
	assert_contains(&body, "public_port must be between");
	let body = f
		.post(
			&save,
			&saved(json!({ "public_port": "15433", "cpu_limit": "x" })),
		)
		.await;
	assert_contains(&body, "CPU limit must be a number.");
	assert_eq!(job_kinds(&db, id).await.len(), 2);

	// General shows what is saved, and the form to change it.
	let (_, page) = f.get(&f.url(id)).await;
	assert_contains(&page, "Network and limits");
	assert_contains(&page, "15433");
	assert_contains(&page, "Save changes");
}

#[db_test]
async fn another_teams_backup_target_cannot_be_chosen(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let (_, bob_team) = user_with_team(&f.app, "bob").await;
	let theirs = add_backup_target(&db, 8002, bob_team, "bobs").await;

	let body = f
		.post(
			&format!("{}/settings", f.url(id)),
			&saved(json!({ "backup_target_id": theirs.to_string() })),
		)
		.await;
	assert_contains(&body, "That no longer exists.");
	assert_eq!(backup_settings(&db, id).await.0, None);
	let (_, page) = f.get(&format!("{}/backups", f.url(id))).await;
	assert_not_contains(&page, "bobs");
}

#[db_test]
async fn deleting_needs_the_name_and_purges_only_when_asked(db: PgPool) {
	let f = Fixture::new(&db).await;
	let keep = f.create(&db, "keep-data", json!({})).await;
	let purge = f.create(&db, "purge-data", json!({})).await;
	let wrong = f.create(&db, "wrong", json!({})).await;

	let body = f
		.post(
			&format!("{}/delete", f.url(wrong)),
			&json!({ "confirm": "WRONG", "purge": true }),
		)
		.await;
	assert_contains(&body, "The name doesn't match.");
	assert_eq!(status_of(&db, wrong).await, "pending");
	assert_eq!(job_kinds(&db, wrong).await, ["database_provision"]);

	// The checkbox left alone: the data volume stays.
	let body = f
		.post(
			&format!("{}/delete", f.url(keep)),
			&json!({ "confirm": "keep-data", "purge": false }),
		)
		.await;
	assert_contains(
		&body,
		&format!(
			"window.location.assign('/teams/{}/projects/{}')",
			f.team, f.project
		),
	);
	assert_eq!(status_of(&db, keep).await, "deleting");
	assert_eq!(
		job_kinds(&db, keep).await,
		["database_provision", "database_remove"]
	);

	// A request that doesn't mention the box at all doesn't purge either.
	let omitted = f.create(&db, "omitted", json!({})).await;
	f.post(
		&format!("{}/delete", f.url(omitted)),
		&json!({ "confirm": "omitted" }),
	)
	.await;
	assert_eq!(
		job_kinds(&db, omitted).await,
		["database_provision", "database_remove"]
	);

	f.post(
		&format!("{}/delete", f.url(purge)),
		&json!({ "confirm": "purge-data", "purge": true }),
	)
	.await;
	assert_eq!(
		job_kinds(&db, purge).await,
		["database_provision", "database_purge"]
	);
	assert_eq!(status_of(&db, purge).await, "deleting");

	// The Advanced tab holds the danger zone; the dialog holds the purge choice.
	let (_, page) = f.get(&format!("{}/advanced", f.url(wrong))).await;
	assert_contains(&page, "Danger zone");
	assert_contains(&page, "Delete database");
	assert_contains(&page, "Also delete the data");
}

#[db_test]
async fn another_teams_database_is_the_404_page_and_cannot_be_changed(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let password = f.password(id).await;
	set_state(&db, id, "running", Some("c1")).await;
	set_jobs(&db, id, "succeeded").await;

	let (bob_token, bob_team) = user_with_team(&f.app, "bob").await;
	let bob = ui_login(&f.app, "bob").await;
	let (_, bob_project) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob_token),
		Some(json!({ "name": "Mine", "slug": "mine" })),
	)
	.await;
	let bob_project = bob_project["id"].as_i64().unwrap();

	// Bob's own team and project, Alice's database id; and Alice's team.
	let bases = [
		format!("/teams/{bob_team}/projects/{bob_project}/databases/{id}"),
		format!("/teams/{bob_team}/projects/{}/databases/{id}", f.project),
		format!("/teams/{}/projects/{}/databases/{id}", f.team, f.project),
	];
	for base in &bases {
		for tail in ["", "/connection", "/backups", "/logs", "/advanced"] {
			let (s, _, page) = get_html(&f.app, &format!("{base}{tail}"), Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{base}{tail}");
			assert_contains(&page, "Not found");
			assert_not_contains(&page, "orders");
			assert_not_contains(&page, &password);
		}
		// Streams and polls answer a Datastar message, never data.
		for tail in [
			"/header",
			"/job-log",
			"/logs/stream",
			"/backups/list",
			"/connection/hide",
		] {
			let (_, _, body) = get_html(&f.app, &format!("{base}{tail}"), Some(&bob)).await;
			assert_not_contains(&body, "orders");
			assert_not_contains(&body, &password);
			assert_not_contains(&body, "db-header");
			assert_not_contains(&body, "db-secret");
		}
		for (tail, signals) in [
			("/reveal", json!({})),
			("/start", json!({})),
			("/stop", json!({})),
			("/restart", json!({})),
			("/settings", saved(json!({ "public_port": "15432" }))),
			(
				"/schedule",
				saved(json!({ "backup_schedule": "0 3 * * *" })),
			),
			("/delete", json!({ "confirm": "orders", "purge": true })),
		] {
			let (_, _, body) =
				post_signals(&f.app, &format!("{base}{tail}"), Some(&bob), &signals).await;
			assert_not_contains(&body, &password);
			assert_not_contains(&body, "window.location");
			assert!(
				body.contains("That no longer exists.")
					|| body.contains("permission")
					|| body.contains("Pick"),
				"{base}{tail}: {body}"
			);
		}
	}
	let (s, _, _) = get_html(
		&f.app,
		&format!("/teams/{bob_team}/projects/{bob_project}/databases/new"),
		Some(&bob),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "bob may create in his own team");
	let forged = f.form("planted", json!({}));
	let (_, _, body) = post_signals(
		&f.app,
		&format!("/teams/{bob_team}/projects/{}/databases", f.project),
		Some(&bob),
		&forged,
	)
	.await;
	assert_contains(&body, "That no longer exists.");

	assert_eq!(count(&db, "databases").await, 1);
	assert_eq!(status_of(&db, id).await, "running");
	assert_eq!(job_kinds(&db, id).await, ["database_provision"]);
	assert_eq!(backup_settings(&db, id).await, (None, None, 7, false));
}

#[db_test]
async fn names_are_escaped(db: PgPool) {
	let f = Fixture::new(&db).await;
	let name = "<script>alert(1)</script>";
	let id = f.create(&db, name, json!({})).await;
	let projects = format!("/teams/{}/projects/{}", f.team, f.project);
	for uri in [
		f.url(id),
		format!("{}/connection", f.url(id)),
		format!("{}/backups", f.url(id)),
		format!("{}/logs", f.url(id)),
		format!("{}/advanced", f.url(id)),
		projects,
	] {
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
		assert_not_contains(&page, "<script>alert(1)");
	}
	let (_, _, body) = get_html(&f.app, &format!("{}/header", f.url(id)), Some(&f.cookie)).await;
	assert_not_contains(&body, "<script>alert(1)");
}

#[db_test]
async fn the_environment_lists_databases_and_polls_while_they_change(db: PgPool) {
	let f = Fixture::new(&db).await;
	let overview = format!("/teams/{}/projects/{}", f.team, f.project);
	let (_, page) = f.get(&overview).await;
	assert_contains(&page, "Nothing in production yet");
	// The database engines are on the create-resource page.
	let (_, picker) = f.get(&format!("{overview}/production/create")).await;
	assert_contains(&picker, "MySQL");

	let id = f.create(&db, "orders", json!({ "engine": "mysql" })).await;
	let (_, page) = f.get(&overview).await;
	assert_contains(&page, &format!("href=\"{}\"", f.url(id)));
	assert_contains(&page, "MySQL 9");
	assert_contains(&page, "Pending");
	// The environment grid polls its own fragment while a database is changing.
	assert_contains(&page, &format!("{overview}/production/resources"));
	assert_contains(&page, "data-on-interval__duration.3s");

	set_state(&db, id, "running", Some("c1")).await;
	let (_, page) = f.get(&overview).await;
	assert_contains(&page, "Running");
	assert_not_contains(&page, "data-on-interval");

	let carol = f.member("carol").await;
	let (_, _, page) = get_html(&f.app, &overview, Some(&carol)).await;
	assert_contains(&page, "orders");
	assert_not_contains(&page, "Create a resource");
}

#[db_test]
async fn the_logs_tab_streams_only_when_there_is_a_container(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let logs = format!("{}/logs", f.url(id));

	let (_, page) = f.get(&logs).await;
	assert_contains(&page, "There is no container to read a log from.");
	assert_not_contains(&page, "id=\"container-log\"");
	let (_, _, body) = get_html(&f.app, &format!("{logs}/stream"), Some(&f.cookie)).await;
	assert_contains(&body, "database is not running");
	assert_contains(&body, "_live");

	set_state(&db, id, "running", Some("c1")).await;
	let (_, page) = f.get(&logs).await;
	assert_contains(&page, "id=\"container-log\"");
	assert_contains(&page, &format!("{logs}/stream"));

	// Without a job General says so instead of opening a stream.
	delete_jobs(&db, id).await;
	let (_, page) = f.get(&f.url(id)).await;
	assert_contains(&page, "No jobs have run yet.");
	assert_not_contains(&page, "id=\"job-log\"");
	let (_, _, body) = get_html(&f.app, &format!("{}/job-log", f.url(id)), Some(&f.cookie)).await;
	assert_contains(&body, "_live");
	add_job(&db, id, "database_start", "running").await;
	let (_, page) = f.get(&f.url(id)).await;
	assert_contains(&page, "id=\"job-log\"");
}

#[db_test]
async fn the_old_settings_page_is_gone_and_its_posts_still_work(db: PgPool) {
	let f = Fixture::new(&db).await;
	let id = f.create(&db, "orders", json!({})).await;
	let (s, _) = f.get(&format!("{}/settings", f.url(id))).await;
	assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
}

#[db_test]
async fn the_json_api_keeps_its_behaviour(db: PgPool) {
	let f = Fixture::new(&db).await;
	let api = format!("/api/v1/teams/{}/projects/{}/databases", f.team, f.project);
	let (s, created) = call(
		&f.app,
		"POST",
		&api,
		Some(&f.token),
		Some(json!({
			"name": "main", "engine": "postgres", "server_id": f.server,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(created["status"], "pending");
	assert_eq!(created["version"], "18");
	assert!(created.get("connection").is_none());
	let id = created["id"].as_i64().unwrap();

	let (s, one) = call(&f.app, "GET", &format!("{api}/{id}"), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert!(one["connection"]["password"].as_str().unwrap().len() >= 16);
	assert!(
		one["connection"]["internal_url"]
			.as_str()
			.unwrap()
			.starts_with("postgres://podkit:")
	);

	let (s, _) = call(
		&f.app,
		"PUT",
		&format!("{api}/{id}/settings"),
		Some(&f.token),
		Some(json!({ "backup_schedule": "nope" })),
	)
	.await;
	assert!(s.is_client_error(), "{s}");
	let (s, updated) = call(
		&f.app,
		"PUT",
		&format!("{api}/{id}/settings"),
		Some(&f.token),
		Some(json!({ "backup_schedule": "0 3 * * *" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert!(updated["backup_next_run_at"].as_i64().is_some());
	let (s, _) = call(
		&f.app,
		"PUT",
		&format!("{api}/{id}/settings"),
		Some(&f.token),
		Some(json!({ "public_port": 15432 })),
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED);

	let (s, list) = call(&f.app, "GET", &api, Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);
	assert!(list[0].get("connection").is_none());

	let (s, _) = call(
		&f.app,
		"DELETE",
		&format!("{api}/{id}?purge_data=true"),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(
		job_kinds(&db, id).await,
		["database_provision", "database_provision", "database_purge"]
	);
	assert_eq!(status_of(&db, id).await, "deleting");
}
