//! Scheduled-task pages: the list, the Cron tab, the forms, the run history, escaping,
//! permissions and tenancy.
//! `cargo test -p podkit-ui --all-features -- cron::`

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};
use crate::tests::{PgPool, app, call, count, cron_ui, db_test, user_with_team};

struct Fx {
	app: Router,
	token: String,
	cookie: String,
	team: i64,
	project: i64,
	env: i64,
	application: i64,
	server: i64,
}

impl Fx {
	async fn new(db: &PgPool) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let cookie = ui_login(&app, "alice").await;
		let server = crate::tests::local_server_id(db, team).await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "Shop", "slug": "shop" })),
		)
		.await;
		assert!(s.is_success(), "{project}");
		let project = project["id"].as_i64().unwrap();
		let (s, application) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/applications"),
			Some(&token),
			Some(json!({
				"name": "shop", "slug": "shop", "server_id": server,
				"repo_url": "https://github.com/acme/shop", "git_ref": "main",
				"container_port": 8080,
			})),
		)
		.await;
		assert!(s.is_success(), "{application}");
		let application = application["id"].as_i64().unwrap();
		let env = crate::tests::scalar(
			db,
			"SELECT environment_id FROM applications WHERE id = $1",
			Some(application),
		)
		.await;
		Self {
			app,
			token,
			cookie,
			team,
			project,
			env,
			application,
			server,
		}
	}

	fn base(&self) -> String {
		format!("/teams/{}/projects/{}/cron", self.team, self.project)
	}

	fn api(&self) -> String {
		format!("/api/v1/teams/{}/projects/{}/cron", self.team, self.project)
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

	fn form(&self, name: &str) -> Value {
		json!({
			"environment": self.env.to_string(), "kind": "exec",
			"application_id": self.application.to_string(), "server_id": "",
			"name": name, "image": "", "command": "php artisan schedule:run",
			"schedule": "*/5 * * * *", "timezone": "Europe/Berlin",
			"timeout_s": "300", "concurrency": "skip", "max_retries": "0",
			"retry_backoff_s": "60", "enabled": true,
		})
	}

	async fn task(&self, name: &str) -> i64 {
		let (s, task) = call(
			&self.app,
			"POST",
			&self.api(),
			Some(&self.token),
			Some(json!({
				"environment_id": self.env, "name": name, "kind": "exec",
				"application_id": self.application, "command": "php artisan schedule:run",
				"schedule": "* * * * *", "timezone": "Europe/Berlin",
			})),
		)
		.await;
		assert_eq!(s, StatusCode::CREATED, "{task}");
		task["id"].as_i64().unwrap()
	}

	/// A member with the built-in read-only role, signed in.
	async fn reader(&self, name: &str) -> String {
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

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_list_is_empty_then_shows_a_task_with_its_next_run_in_its_zone(db: PgPool) {
	let f = Fx::new(&db).await;
	let (s, page) = f.get(&f.base()).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "No scheduled tasks yet.");
	assert_contains(&page, "Command in an app");
	assert_contains(&page, "One-shot container");
	assert_eq!(page_main(&page).matches("<h1").count(), 1);

	let task = f.task("Laravel scheduler").await;
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "Laravel scheduler");
	assert_contains(&page, &format!("{}/{task}", f.base()));
	assert_contains(&page, "Europe/Berlin");
	assert_contains(&page, "Never run");
	// Read in Berlin, so the abbreviation is CET or CEST rather than UTC.
	assert!(page.contains("CET") || page.contains("CEST"), "{page}");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_cron_tab_lists_the_apps_tasks_and_offers_to_add_one(db: PgPool) {
	let f = Fx::new(&db).await;
	let app_page = format!(
		"/teams/{}/projects/{}/apps/{}",
		f.team, f.project, f.application
	);
	let tab = format!("{app_page}/cron");
	let (s, page) = f.get(&tab).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "No scheduled commands.");
	assert_contains(&page, "Add a task");
	assert_contains(
		&page,
		&format!("kind=exec&amp;application={}", f.application),
	);
	// It is one of the tabs.
	let (_, deployments) = f.get(&app_page).await;
	assert_contains(&deployments, &tab);

	f.task("Laravel scheduler").await;
	let (_, page) = f.get(&tab).await;
	assert_contains(&page, "Laravel scheduler");
	assert_not_contains(&page, "No scheduled commands.");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_create_form_is_fixed_to_its_kind_and_the_picker_offers_it(db: PgPool) {
	let f = Fx::new(&db).await;
	let (s, page) = f
		.get(&format!(
			"{}/new?environment={}&kind=exec&application={}",
			f.base(),
			f.env,
			f.application
		))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "New scheduled command");
	assert_contains(&page, "Command, run in the app");
	assert_contains(&page, "Next runs, in UTC");
	assert_contains(&page, "Daily at 03:00");
	assert_not_contains(&page, "Based on an application");
	assert_eq!(page_main(&page).matches("<h1").count(), 1);

	let (_, page) = f
		.get(&format!(
			"{}/new?environment={}&kind=container",
			f.base(),
			f.env
		))
		.await;
	assert_contains(&page, "New one-shot container");
	assert_contains(&page, "Based on an application (optional)");
	assert_contains(&page, "docker.io/library/busybox");

	let slug = cron_ui::env_slug(&db, f.env).await;
	let (s, picker) = f
		.get(&format!(
			"/teams/{}/projects/{}/{slug}/create",
			f.team, f.project
		))
		.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&picker, "Scheduled task");
	assert_contains(&picker, "kind=container");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn creating_a_task_from_the_form_stores_it_and_a_bad_field_says_why(db: PgPool) {
	let f = Fx::new(&db).await;
	let body = f.post(&f.base(), &f.form("Laravel scheduler")).await;
	assert_contains(&body, "/cron/");
	assert_eq!(count(&db, "cron_tasks").await, 1);

	let mut bad = f.form("Broken");
	bad["schedule"] = json!("every day");
	let body = f.post(&f.base(), &bad).await;
	assert_eq!(count(&db, "cron_tasks").await, 1);
	assert!(!body.contains("/cron/"), "{body}");

	let mut zone = f.form("Wrong zone");
	zone["timezone"] = json!("Mars/Olympus");
	let body = f.post(&f.base(), &zone).await;
	assert_contains(&body, "not a known time zone");
	assert_eq!(count(&db, "cron_tasks").await, 1);

	let mut text = f.form("Not a number");
	text["timeout_s"] = json!("soon");
	let body = f.post(&f.base(), &text).await;
	assert_contains(&body, "must be a whole number");
	assert_eq!(count(&db, "cron_tasks").await, 1);

	let mut container = f.form("Cleanup");
	container["kind"] = json!("container");
	container["application_id"] = json!("");
	container["image"] = json!("docker.io/library/busybox");
	container["server_id"] = json!(f.server.to_string());
	f.post(&f.base(), &container).await;
	assert_eq!(count(&db, "cron_tasks").await, 2);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_preview_shows_five_runs_in_the_zone_and_an_error_for_a_bad_expression(db: PgPool) {
	let f = Fx::new(&db).await;
	let body = f
		.post(
			&format!("{}/preview", f.base()),
			&json!({ "schedule": "0 3 * * *", "timezone": "America/New_York" }),
		)
		.await;
	assert_contains(&body, "Next runs, in America/New_York");
	assert_eq!(body.matches("<li>").count(), 5, "{body}");
	assert!(body.contains("EST") || body.contains("EDT"), "{body}");

	let body = f
		.post(
			&format!("{}/preview", f.base()),
			&json!({ "schedule": "nope", "timezone": "UTC" }),
		)
		.await;
	assert_contains(&body, "Not a valid schedule");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_task_name_with_a_script_is_escaped_everywhere(db: PgPool) {
	let f = Fx::new(&db).await;
	let task = f.task("<script>alert(1)</script>").await;
	for uri in [
		f.base(),
		format!("{}/{task}", f.base()),
		format!(
			"/teams/{}/projects/{}/apps/{}/cron",
			f.team, f.project, f.application
		),
	] {
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_not_contains(&page, "<script>alert(1)");
		assert_contains(&page, "&lt;script&gt;alert(1)");
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_task_page_shows_settings_runs_and_the_log_of_a_run(db: PgPool) {
	let f = Fx::new(&db).await;
	let task = f.task("Laravel scheduler").await;
	let page_url = format!("{}/{task}", f.base());
	let (s, page) = f.get(&page_url).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Laravel scheduler");
	assert_contains(&page, "No runs yet.");
	assert_contains(&page, "Run now");
	assert_contains(&page, "Unsaved changes");
	assert_contains(&page, "Next runs, in Europe/Berlin");
	assert_eq!(page_main(&page).matches("<h1").count(), 1);
	assert_not_contains(&page, "cron-log-box");

	// Run now queues a run; the history then lists it and the page offers its log.
	let body = f.post(&format!("{page_url}/run"), &json!({})).await;
	assert_contains(&body, "Run queued");
	assert_eq!(count(&db, "cron_runs").await, 1);
	let (_, page) = f.get(&page_url).await;
	assert_contains(&page, "cron-log-box");
	assert_contains(&page, "Queued");

	// A second press while the first is waiting is refused by the default policy.
	let body = f.post(&format!("{page_url}/run"), &json!({})).await;
	assert_contains(&body, "already running");
	assert_eq!(count(&db, "cron_runs").await, 1);

	// The history re-fetches itself while a run is going.
	let (_, _, body) = get_html(&f.app, &format!("{page_url}/runs"), Some(&f.cookie)).await;
	assert_contains(&body, "cron-runs");
	assert_contains(&body, "data-on-interval");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn saving_pausing_and_deleting_go_through_the_service(db: PgPool) {
	let f = Fx::new(&db).await;
	let task = f.task("Laravel scheduler").await;
	let url = format!("{}/{task}", f.base());

	let mut changed = f.form("Renamed");
	changed["schedule"] = json!("0 4 * * *");
	changed["concurrency"] = json!("queue");
	changed["max_retries"] = json!(2);
	let body = f.post(&url, &changed).await;
	assert_contains(&body, "Task saved");
	let (name, schedule, policy, retries) = cron_ui::settings(&db, task).await;
	assert_eq!(
		(name.as_str(), schedule.as_str(), policy.as_str(), retries),
		("Renamed", "0 4 * * *", "queue", 2)
	);

	let mut broken = f.form("Renamed");
	broken["schedule"] = json!("not cron");
	f.post(&url, &broken).await;
	let schedule = cron_ui::settings(&db, task).await.1;
	assert_eq!(schedule, "0 4 * * *", "a bad expression changes nothing");

	f.post(&format!("{url}/pause"), &json!({})).await;
	assert!(!cron_ui::enabled(&db, task).await);
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "Paused");
	f.post(&format!("{url}/resume"), &json!({})).await;

	f.post(&format!("{url}/delete"), &json!({})).await;
	assert_eq!(count(&db, "cron_tasks").await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_reader_sees_tasks_but_no_controls_and_forged_posts_are_refused(db: PgPool) {
	let f = Fx::new(&db).await;
	let task = f.task("Laravel scheduler").await;
	let cookie = f.reader("rita").await;
	let url = format!("{}/{task}", f.base());

	let (s, _, page) = get_html(&f.app, &url, Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Laravel scheduler");
	for hidden in ["Run now", "Pause", "Delete task", "Unsaved changes"] {
		assert_not_contains(page_main(&page), hidden);
	}
	let (_, _, list) = get_html(&f.app, &f.base(), Some(&cookie)).await;
	assert_not_contains(page_main(&list), "Command in an app");
	let (s, _, _) = get_html(
		&f.app,
		&format!("{}/new?environment={}&kind=exec", f.base(), f.env),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);

	for (uri, signals) in [
		(f.base(), f.form("Forged")),
		(url.clone(), f.form("Forged")),
		(format!("{url}/run"), json!({})),
		(format!("{url}/pause"), json!({})),
		(format!("{url}/delete"), json!({})),
	] {
		let body = post_signals(&f.app, &uri, Some(&cookie), &signals).await.2;
		assert!(
			body.contains("permission") || body.contains("don't have"),
			"{uri}: {body}"
		);
	}
	assert_eq!(count(&db, "cron_tasks").await, 1);
	assert_eq!(count(&db, "cron_runs").await, 0);
	let name = cron_ui::settings(&db, task).await.0;
	assert_eq!(name, "Laravel scheduler");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn another_teams_task_is_a_404_page_and_its_posts_change_nothing(db: PgPool) {
	let f = Fx::new(&db).await;
	let task = f.task("Laravel scheduler").await;
	let (mtoken, mteam) = user_with_team(&f.app, "mallory").await;
	let mcookie = ui_login(&f.app, "mallory").await;
	let (_, mproject) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{mteam}/projects"),
		Some(&mtoken),
		Some(json!({ "name": "Theirs", "slug": "theirs" })),
	)
	.await;
	let mproject = mproject["id"].as_i64().unwrap();

	let theirs = format!("/teams/{mteam}/projects/{mproject}/cron/{task}");
	let (s, _, page) = get_html(&f.app, &theirs, Some(&mcookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&page, "Laravel scheduler");
	// Her own team id with his project: also not found.
	let (s, _, _) = get_html(
		&f.app,
		&format!("/teams/{mteam}/projects/{}/cron", f.project),
		Some(&mcookie),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// And his team is closed to her.
	let (s, _, _) = get_html(&f.app, &format!("{}/{task}", f.base()), Some(&mcookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);

	for action in ["", "/run", "/pause", "/delete", "/runs/1/cancel"] {
		post_signals(
			&f.app,
			&format!("{theirs}{action}"),
			Some(&mcookie),
			&f.form("Hijacked"),
		)
		.await;
	}
	let (_, _, log) = get_html(&f.app, &format!("{theirs}/runs/1/log"), Some(&mcookie)).await;
	assert_not_contains(&log, "Laravel");
	assert_eq!(count(&db, "cron_tasks").await, 1);
	assert_eq!(count(&db, "cron_runs").await, 0);
	let name = cron_ui::settings(&db, task).await.0;
	assert_eq!(name, "Laravel scheduler");
}
