//! Tests of the S3 destinations page and the Backups tab of a database. Each test keeps the
//! name of the pre-redesign test it replaces; assertions moved with the markup.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::history::human_size;
use crate::testing::{assert_contains, assert_not_contains, get_html, post_signals, ui_login};
use crate::tests::dbs::{
	add_backup, add_backup_target, backup_count, job_kinds, local_server, set_backup_target,
	set_state, stored_secret, target_count, user_id,
};
use crate::tests::{PgPool, app, call, db_test, user_with_team};

const SECRET: &str = "s3cr3t-Access-Key-4711";

struct Fixture {
	app: Router,
	token: String,
	cookie: String,
	team: i64,
	project: i64,
	database: i64,
	target: i64,
}

impl Fixture {
	/// Alice, her team, a project, a running database "orders" with a
	/// backup target set.
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
		let project = project["id"].as_i64().unwrap();
		let server = local_server(db, team).await;
		let (s, database) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects/{project}/databases"),
			Some(&token),
			Some(json!({ "name": "orders", "engine": "postgres", "server_id": server })),
		)
		.await;
		assert!(s.is_success(), "create database: {s} {database}");
		let database = database["id"].as_i64().unwrap();
		set_state(db, database, "running", Some("c1")).await;
		let target = add_backup_target(db, 900, team, "offsite").await;
		set_backup_target(db, database, target).await;
		Self {
			app,
			token,
			cookie,
			team,
			project,
			database,
			target,
		}
	}

	fn targets(&self) -> String {
		format!("/teams/{}/backup-targets", self.team)
	}

	fn base(&self) -> String {
		format!(
			"/teams/{}/projects/{}/databases/{}/backups",
			self.team, self.project, self.database
		)
	}

	fn api(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/databases/{}/backups",
			self.team, self.project, self.database
		)
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

#[allow(clippy::needless_pass_by_value)]
fn form(extra: Value) -> Value {
	let mut form = json!({
		"name": "offsite", "endpoint": "https://s3.example.com", "region": "eu-west-1",
		"bucket": "podkit", "prefix": "", "path_style": false,
		"access_key_id": "AKIA1234", "secret_access_key": SECRET,
	});
	for (k, v) in extra.as_object().unwrap() {
		form[k] = v.clone();
	}
	form
}

#[test]
fn sizes_read_as_people_write_them() {
	for (bytes, text) in [
		(0, "0 B"),
		(1023, "1023 B"),
		(1024, "1.0 KB"),
		(1536 * 1024, "1.5 MB"),
		(5 * 1024 * 1024 * 1024, "5.0 GB"),
		(3 * 1024_i64.pow(4), "3.0 TB"),
		(2048 * 1024_i64.pow(4), "2048.0 TB"),
	] {
		assert_eq!(human_size(bytes), text, "{bytes}");
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn targets_are_listed_added_and_never_show_the_secret(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, page) = f.get(&f.targets()).await;
	assert_eq!(s, StatusCode::OK);
	for text in [
		"offsite",
		"https://s3.example.com",
		"Bucket",
		"us-east-1",
		"Add a destination",
	] {
		assert_contains(&page, text);
	}

	let body = f
		.post(
			&f.targets(),
			&form(json!({ "name": "second", "prefix": "podkit/prod", "path_style": true })),
		)
		.await;
	assert_contains(&body, &format!("window.location.assign('{}')", f.targets()));
	assert_not_contains(&body, SECRET);

	let (_, page) = f.get(&f.targets()).await;
	for text in [
		"second",
		"AKIA1234",
		"eu-west-1",
		"podkit",
		"podkit/prod",
		"path-style",
	] {
		assert_contains(&page, text);
	}
	assert_not_contains(&page, SECRET);
	// The page has the form's secret field empty and nothing after it.
	assert_not_contains(&page, "secret_access_key\":\"s");
	// Stored encrypted, not as typed.
	assert!(!String::from_utf8_lossy(&stored_secret(&db).await).contains(SECRET));

	let (s, list) = call(
		&f.app,
		"GET",
		&format!("/api/v1/teams/{}/backup-targets", f.team),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 2);
	assert_not_contains(&list.to_string(), SECRET);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn bad_target_input_is_an_inline_message_and_adds_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	for (extra, message) in [
		(
			json!({ "name": "x", "endpoint": "http://s3.example.com" }),
			"endpoint must use https",
		),
		(
			json!({ "name": "x", "endpoint": "nope" }),
			"That endpoint isn't valid.",
		),
		(
			json!({ "name": "x", "secret_access_key": "" }),
			"That secret access key isn't valid.",
		),
		(
			json!({ "name": "x", "prefix": "../up" }),
			"That prefix isn't valid.",
		),
		(json!({ "name": "offsite" }), "already exists"),
	] {
		let body = f.post(&f.targets(), &form(extra)).await;
		assert_contains(&body, message);
		assert_not_contains(&body, "window.location");
		assert_not_contains(&body, SECRET);
	}
	assert_eq!(target_count(&db).await, 1);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn deleting_a_target_works_and_another_teams_target_is_untouchable(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (_, mallory_team) = user_with_team(&f.app, "mallory").await;
	let theirs = add_backup_target(&db, 901, mallory_team, "theirs").await;

	let body = f
		.post(&format!("{}/{theirs}/delete", f.targets()), &json!({}))
		.await;
	assert_contains(&body, "_delete_error");
	assert_contains(&body, "That no longer exists.");
	assert_eq!(target_count(&db).await, 2);

	let body = f
		.post(&format!("{}/{}/delete", f.targets(), f.target), &json!({}))
		.await;
	assert_contains(&body, &format!("window.location.assign('{}')", f.targets()));
	assert_eq!(target_count(&db).await, 1);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn targets_of_another_team_are_a_404_page(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (_, mallory_team) = user_with_team(&f.app, "mallory").await;
	add_backup_target(&db, 901, mallory_team, "theirs").await;

	let (s, page) = f
		.get(&format!("/teams/{mallory_team}/backup-targets"))
		.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&page, "theirs");
	let body = f
		.post(
			&format!("/teams/{mallory_team}/backup-targets"),
			&form(json!({ "name": "evil" })),
		)
		.await;
	assert_not_contains(&body, "window.location");
	assert_eq!(target_count(&db).await, 2);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_target_name_with_a_script_is_escaped(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_backup_target(&db, 902, f.team, "<script>alert(1)</script>").await;
	let (_, page) = f.get(&f.targets()).await;
	assert_not_contains(&page, "<script>alert(1)");
	assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_read_only_member_sees_targets_but_cannot_change_them(db: PgPool) {
	let f = Fixture::new(&db).await;
	let cookie = f.reader("rita").await;

	let (s, _, page) = get_html(&f.app, &f.targets(), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "offsite");
	assert_not_contains(&page, "Add destination");
	assert_not_contains(&page, "Delete");

	let body = post_signals(
		&f.app,
		&f.targets(),
		Some(&cookie),
		&form(json!({ "name": "forged" })),
	)
	.await
	.2;
	assert_contains(&body, "You don't have permission to do that.");
	let body = post_signals(
		&f.app,
		&format!("{}/{}/delete", f.targets(), f.target),
		Some(&cookie),
		&json!({}),
	)
	.await
	.2;
	assert_contains(&body, "You don't have permission to do that.");
	assert_eq!(target_count(&db).await, 1);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_tab_lists_backups_with_size_who_and_the_error(db: PgPool) {
	let f = Fixture::new(&db).await;
	let alice = user_id(&db, "alice").await;
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"succeeded",
		Some(1536 * 1024),
		None,
		Some(alice),
	)
	.await;
	add_backup(
		&db,
		11,
		f.database,
		Some(f.target),
		"failed",
		None,
		Some("bucket <b>gone</b>"),
		None,
	)
	.await;

	let (s, page) = f.get(&f.base()).await;
	assert_eq!(s, StatusCode::OK);
	for text in [
		"Backups",
		"Succeeded",
		"1.5 MB",
		"Failed",
		"Back up now",
		"data-label=\"Trigger\"",
		"manual \u{b7} alice",
		">scheduled<",
	] {
		assert_contains(&page, text);
	}
	assert_contains(&page, "bucket &lt;b&gt;gone&lt;/b&gt;");
	// All finished: the list does not poll (the header still does while the database is new).
	assert_not_contains(&page, &format!("{}/list", f.base()));

	add_backup(
		&db,
		12,
		f.database,
		Some(f.target),
		"running",
		None,
		None,
		Some(alice),
	)
	.await;
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "data-on-interval__duration.3s");
	assert_contains(&page, &format!("{}/list", f.base()));
	let (_, _, patch) = get_html(&f.app, &format!("{}/list", f.base()), Some(&f.cookie)).await;
	assert_contains(&patch, "Running");
	assert_contains(&patch, "data-on-interval");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn back_up_now_queues_a_backup_and_the_list_starts_polling(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f.post(&f.base(), &json!({})).await;
	assert_contains(&body, "Queued");
	assert_contains(&body, "data-on-interval");
	assert_eq!(backup_count(&db, f.database).await, 1);

	let (s, list) = call(&f.app, "GET", &f.api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list[0]["status"], "queued");
	assert!(list[0]["triggered_by"].is_i64());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn without_a_target_there_is_no_button_and_a_forged_post_is_refused(db: PgPool) {
	let f = Fixture::new(&db).await;
	crate::tests::dbs::clear_backup_target(&db, f.database).await;
	let (_, page) = f.get(&f.base()).await;
	assert_not_contains(&page, "Back up now");
	assert_contains(&page, "configure a backup target first");

	let body = f.post(&f.base(), &json!({})).await;
	assert_contains(&body, "configure a backup target first");
	assert_eq!(backup_count(&db, f.database).await, 0);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_restore_needs_the_typed_name_even_when_the_post_is_forged(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"succeeded",
		Some(10),
		None,
		None,
	)
	.await;
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "overwrites the current data");
	assert_contains(&page, "<strong class=\"break-all\">orders</strong>");
	assert_contains(&page, "Restore and overwrite");

	let uri = format!("{}/10/restore", f.base());
	for signals in [
		json!({}),
		json!({ "confirm": "" }),
		json!({ "confirm": "order" }),
		json!({ "confirm": "orders " }),
		json!({ "confirm": "ORDERS" }),
	] {
		let body = f.post(&uri, &signals).await;
		assert_contains(&body, "The name doesn't match.");
		assert_not_contains(&body, "Restore queued");
	}
	assert!(job_kinds(&db, 10).await.is_empty());

	let body = f.post(&uri, &json!({ "confirm": "orders" })).await;
	assert_contains(&body, "Restore queued");
	assert_eq!(job_kinds(&db, 10).await, ["restore"]);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_restore_of_an_unsuitable_backup_is_refused(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"failed",
		None,
		Some("x"),
		None,
	)
	.await;
	add_backup(
		&db,
		11,
		f.database,
		Some(f.target),
		"succeeded",
		Some(1),
		None,
		None,
	)
	.await;
	let confirm = json!({ "confirm": "orders" });

	let body = f.post(&format!("{}/10/restore", f.base()), &confirm).await;
	assert_contains(&body, "only a succeeded backup can be restored");
	let body = f.post(&format!("{}/99/restore", f.base()), &confirm).await;
	assert_contains(&body, "That no longer exists.");

	set_state(&db, f.database, "stopped", None).await;
	let body = f.post(&format!("{}/11/restore", f.base()), &confirm).await;
	assert_contains(&body, "the database must be running");
	assert!(job_kinds(&db, 10).await.is_empty() && job_kinds(&db, 11).await.is_empty());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn deleting_a_backup_removes_it_unless_it_is_in_progress(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"failed",
		None,
		Some("x"),
		None,
	)
	.await;
	add_backup(
		&db,
		11,
		f.database,
		Some(f.target),
		"running",
		None,
		None,
		None,
	)
	.await;

	let body = f.post(&format!("{}/11/delete", f.base()), &json!({})).await;
	assert_contains(&body, "backup is in progress");
	assert_eq!(backup_count(&db, f.database).await, 2);

	let body = f.post(&format!("{}/10/delete", f.base()), &json!({})).await;
	assert_contains(&body, "Backup deleted");
	assert_eq!(backup_count(&db, f.database).await, 1);
	assert_not_contains(&body, "Failed");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn another_teams_database_and_backups_are_not_found(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"succeeded",
		Some(1),
		None,
		None,
	)
	.await;
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

	// Her database through his team and project: the 404 page.
	let theirs = format!(
		"/teams/{mteam}/projects/{mproject}/databases/{}/backups",
		f.database
	);
	let (s, _, page) = get_html(&f.app, &theirs, Some(&mcookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&page, "orders");
	// His own team id with her database's path: also not found.
	let (s, page) = f
		.get(&format!(
			"/teams/{}/projects/{mproject}/databases/{}/backups",
			f.team, f.database
		))
		.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&page, "orders");
	// And her team is closed to him.
	let (s, _, _) = get_html(&f.app, &f.base(), Some(&mcookie)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);

	for action in ["", "/10/restore", "/10/delete"] {
		let body = post_signals(
			&f.app,
			&format!("{theirs}{action}"),
			Some(&mcookie),
			&json!({ "confirm": "orders" }),
		)
		.await
		.2;
		assert_not_contains(&body, "Restore queued");
		assert_not_contains(&body, "Queued");
	}
	assert_eq!(backup_count(&db, f.database).await, 1);
	assert!(job_kinds(&db, 10).await.is_empty());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_read_only_member_sees_history_but_no_actions(db: PgPool) {
	let f = Fixture::new(&db).await;
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"succeeded",
		Some(1),
		None,
		None,
	)
	.await;
	let cookie = f.reader("rita").await;

	let (s, _, page) = get_html(&f.app, &f.base(), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Succeeded");
	for hidden in [
		"Back up now",
		"Restore and overwrite",
		">Delete<",
		">Restore<",
	] {
		assert_not_contains(&page, hidden);
	}

	let confirm = json!({ "confirm": "orders" });
	for (uri, signals) in [
		(f.base(), json!({})),
		(format!("{}/10/restore", f.base()), confirm),
		(format!("{}/10/delete", f.base()), json!({})),
	] {
		let body = post_signals(&f.app, &uri, Some(&cookie), &signals).await.2;
		assert_contains(&body, "You don't have permission to do that.");
	}
	assert_eq!(backup_count(&db, f.database).await, 1);
	assert!(job_kinds(&db, 10).await.is_empty());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn saving_a_destination_patches_the_history_without_a_reload(db: PgPool) {
	let f = Fixture::new(&db).await;
	crate::tests::dbs::clear_backup_target(&db, f.database).await;
	let (_, page) = f.get(&f.base()).await;
	assert_not_contains(&page, "Back up now");
	assert_contains(&page, "configure a backup target first");
	assert_contains(&page, "id=\"backup-section\"");
	// The form posts to this tab's own route, which answers with the section.
	assert_contains(&page, &format!("{}/settings", f.base()));

	let signals = |target: &str| {
		json!({
			"backup_target_id": target, "backup_schedule": "", "backup_keep": "7",
			"public_port": "", "memory_limit_mb": "", "cpu_limit": "",
		})
	};
	let body = f
		.post(
			&format!("{}/settings", f.base()),
			&signals(&f.target.to_string()),
		)
		.await;
	assert_contains(&body, "datastar-patch-elements");
	assert_contains(&body, "id=\"backup-section\"");
	assert_contains(&body, "Back up now");
	assert_not_contains(&body, "configure a backup target first");
	assert_not_contains(&body, "window.location");
	assert_contains(&body, "Schedule saved");
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "Back up now");

	// Taking the destination away puts the reason back, again in place.
	let body = f
		.post(&format!("{}/settings", f.base()), &signals(""))
		.await;
	assert_contains(&body, "id=\"backup-section\"");
	assert_contains(&body, "configure a backup target first");
	assert_not_contains(&body, "Back up now");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_reader_cannot_save_the_schedule_with_a_forged_post(db: PgPool) {
	let f = Fixture::new(&db).await;
	let reader = f.reader("rita").await;
	let (_, _, body) = post_signals(
		&f.app,
		&format!("{}/settings", f.base()),
		Some(&reader),
		&json!({
			"backup_target_id": "", "backup_schedule": "0 3 * * *", "backup_keep": "3",
			"public_port": "", "memory_limit_mb": "", "cpu_limit": "",
		}),
	)
	.await;
	assert_contains(&body, "permission");
	assert_not_contains(&body, "backup-section");
	let (_, page) = f.get(&f.base()).await;
	assert_not_contains(&page, "&quot;backup_schedule&quot;:&quot;0 3");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_settings_target_select_links_to_adding_one(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "Add an S3 destination");
	assert_contains(&page, &format!("href=\"{}\"", f.targets()));
	// The destination, the cron field with its preview, and the retention are all here.
	for text in [
		"Destination",
		"Schedule (cron, UTC)",
		"Backups to keep",
		"schedule-hint",
		"offsite",
	] {
		assert_contains(&page, text);
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_json_api_keeps_its_behaviour(db: PgPool) {
	let f = Fixture::new(&db).await;
	// Restore over JSON has never asked for a typed name.
	add_backup(
		&db,
		10,
		f.database,
		Some(f.target),
		"succeeded",
		Some(1),
		None,
		None,
	)
	.await;
	let (s, _) = call(
		&f.app,
		"POST",
		&format!("{}/10/restore", f.api()),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(job_kinds(&db, 10).await, ["restore"]);

	let (s, created) = call(&f.app, "POST", &f.api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::ACCEPTED);
	assert_eq!(created["status"], "queued");
	let (s, list) = call(&f.app, "GET", &f.api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 2);
	let (s, _) = call(
		&f.app,
		"DELETE",
		&format!("{}/10", f.api()),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&f.app,
		"DELETE",
		&format!("{}/10", f.api()),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

/// A stand-in for the storage: answers every request with `status` and returns its endpoint.
async fn fake_storage(status: u16, body: &'static str) -> String {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	tokio::spawn(async move {
		loop {
			let Ok((mut socket, _)) = listener.accept().await else {
				return;
			};
			let mut buf = [0_u8; 4096];
			let _ = socket.read(&mut buf).await;
			let reply = format!(
				"HTTP/1.1 {status} Stand-in\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
				body.len()
			);
			let _ = socket.write_all(reply.as_bytes()).await;
		}
	});
	format!("http://127.0.0.1:{port}")
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn testing_a_destination_says_what_the_storage_answered_and_stores_nothing(db: PgPool) {
	let f = Fixture::new(&db).await;
	let test = format!("{}/test", f.targets());
	for (status, answer, expected, ok) in [
		(
			404,
			"<Error><Code>NoSuchKey</Code></Error>",
			"Connected.",
			true,
		),
		(200, "", "Connected.", true),
		(
			404,
			"<Error><Code>NoSuchBucket</Code></Error>",
			"the bucket does not exist",
			false,
		),
		// Whatever the storage said, the person is told one thing: no key, bucket or status of
		// whatever the endpoint points at.
		(403, "", "podkit could not use that storage", false),
		(500, "", "podkit could not use that storage", false),
	] {
		let endpoint = fake_storage(status, answer).await;
		let body = f
			.post(
				&test,
				&form(json!({ "name": "probe", "endpoint": endpoint, "path_style": true })),
			)
			.await;
		assert_contains(&body, expected);
		assert_contains(&body, &format!("\"_test_ok\":{ok}"));
		assert_not_contains(&body, SECRET);
		assert_not_contains(&body, "window.location");
	}
	// The fields are checked before anything is sent.
	let body = f
		.post(
			&test,
			&form(json!({ "endpoint": "http://s3.example.com", "path_style": true })),
		)
		.await;
	assert_contains(&body, "endpoint must use https");
	let body = f
		.post(&test, &form(json!({ "secret_access_key": "" })))
		.await;
	assert_contains(&body, "That secret access key isn't valid.");
	assert_eq!(target_count(&db).await, 1);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_read_only_member_cannot_test_a_destination(db: PgPool) {
	let f = Fixture::new(&db).await;
	let cookie = f.reader("rita").await;
	let endpoint = fake_storage(404, "").await;
	let body = post_signals(
		&f.app,
		&format!("{}/test", f.targets()),
		Some(&cookie),
		&form(json!({ "endpoint": endpoint, "path_style": true })),
	)
	.await
	.2;
	assert_contains(&body, "You don't have permission to do that.");
	assert_not_contains(&body, "Connected.");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn destinations_show_their_provider_and_the_form_offers_the_providers(db: PgPool) {
	let f = Fixture::new(&db).await;
	for (name, endpoint, path_style) in [
		("aws", "https://s3.eu-west-1.amazonaws.com", false),
		("r2", "https://acct.r2.cloudflarestorage.com", false),
		("garage", "https://s3.garage.example.com", true),
	] {
		let body = f
			.post(
				&f.targets(),
				&form(json!({ "name": name, "endpoint": endpoint, "path_style": path_style })),
			)
			.await;
		assert_contains(&body, "window.location");
	}
	let (_, page) = f.get(&f.targets()).await;
	for text in [
		"data-provider=\"aws\"",
		"data-provider=\"r2\"",
		"data-provider=\"garage\"",
		"data-provider=\"other\"",
		"Amazon S3",
		"Cloudflare R2",
		"Garage",
	] {
		assert_contains(&page, text);
	}
	// The picker: four providers as one radio group, with the brand marks podkit has.
	assert_eq!(page.matches("type=\"radio\" name=\"provider\"").count(), 4);
	assert_contains(&page, "data-logo=\"cloudflare\"");
	assert_contains(&page, "Test connection");
	assert_contains(&page, "Other S3-compatible");
	// No product that no longer ships images is recommended.
	assert!(!page.to_ascii_lowercase().contains("minio"), "{page}");
	assert_not_contains(&page, SECRET);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn deleting_a_destination_asks_first_and_names_it(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (_, page) = f.get(&f.targets()).await;
	assert_contains(&page, "id=\"delete-target-dialog\"");
	assert_contains(&page, "Delete this destination?");
	assert_contains(&page, "aria-label=\"Delete offsite\"");
}

// --- restore drills -------------------------------------------------------------------------

/// A succeeded backup whose dump is stored, so a drill can use it.
async fn stored_backup(db: &PgPool, f: &Fixture, id: i64) {
	add_backup(
		db,
		id,
		f.database,
		Some(f.target),
		"succeeded",
		Some(1),
		None,
		None,
	)
	.await;
	sqlx::query("UPDATE backups SET object_key = 'k', source_tables = 3 WHERE id = $1")
		.bind(id)
		.execute(db)
		.await
		.unwrap();
}

impl Fixture {
	fn drills_api(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/databases/{}/drills",
			self.team, self.project, self.database
		)
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_backups_tab_shows_restore_drills_off_by_default(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, page) = f.get(&f.base()).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Restore drills");
	assert_contains(&page, "No drills yet.");
	assert_contains(&page, "Scheduled drills are off.");
	assert_contains(&page, "id=\"drill-section\"");
	// No backup yet: the button says why instead.
	assert_not_contains(&page, "Run drill now");
	assert_contains(&page, "There is no succeeded backup to drill yet");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn running_a_drill_from_the_tab_queues_it_and_shows_it(db: PgPool) {
	let f = Fixture::new(&db).await;
	stored_backup(&db, &f, 10).await;
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "Run drill now");

	let body = f.post(&format!("{}/drills", f.base()), &json!({})).await;
	assert_contains(&body, "id=\"drill-section\"");
	assert_contains(&body, "Restore drill queued");
	assert_contains(&body, "Queued");
	let kinds: Vec<String> =
		sqlx::query_scalar("SELECT kind FROM jobs WHERE kind = 'restore_drill'")
			.fetch_all(&db)
			.await
			.unwrap();
	assert_eq!(kinds, ["restore_drill"]);
	// The database is not touched: no restore of the backup, and nothing queued for the
	// database beyond the provision from its creation (no stop).
	assert!(job_kinds(&db, 10).await.is_empty());
	assert_eq!(job_kinds(&db, f.database).await, ["database_provision"]);

	// A second one while the first is queued is refused, in the card.
	let body = f.post(&format!("{}/drills", f.base()), &json!({})).await;
	assert_contains(&body, "already queued or running");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_drill_api_lists_runs_and_schedules(db: PgPool) {
	let f = Fixture::new(&db).await;
	let (s, body) = call(&f.app, "POST", &f.drills_api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "no backup yet: {body}");

	stored_backup(&db, &f, 10).await;
	let (s, drill) = call(&f.app, "POST", &f.drills_api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::ACCEPTED, "{drill}");
	assert_eq!(drill["status"], "queued");
	assert_eq!(drill["backup_id"], 10);
	assert_eq!(drill["source_tables"], 3);
	let (s, again) = call(&f.app, "POST", &f.drills_api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::CONFLICT, "{again}");

	let (s, list) = call(&f.app, "GET", &f.drills_api(), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);

	let schedule = format!("{}/schedule", f.drills_api());
	let (s, off) = call(&f.app, "GET", &schedule, Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(off, json!({ "schedule": null, "next_run_at": null }));
	let (s, on) = call(
		&f.app,
		"PUT",
		&schedule,
		Some(&f.token),
		Some(json!({ "schedule": " 0 4 * * 0 " })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{on}");
	assert_eq!(on["schedule"], "0 4 * * 0");
	assert!(on["next_run_at"].is_string());
	let (s, bad) = call(
		&f.app,
		"PUT",
		&schedule,
		Some(&f.token),
		Some(json!({ "schedule": "weekly" })),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
	let (s, cleared) = call(
		&f.app,
		"PUT",
		&schedule,
		Some(&f.token),
		Some(json!({ "schedule": null })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(cleared, json!({ "schedule": null, "next_run_at": null }));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_reader_sees_drills_but_cannot_run_or_schedule_them(db: PgPool) {
	let f = Fixture::new(&db).await;
	stored_backup(&db, &f, 10).await;
	let cookie = f.reader("rita").await;
	let (s, _, page) = get_html(&f.app, &f.base(), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Restore drills");
	assert_not_contains(&page, "Run drill now");
	assert_not_contains(&page, "Save drill schedule");

	for (uri, signals) in [
		(format!("{}/drills", f.base()), json!({})),
		(
			format!("{}/drills/schedule", f.base()),
			json!({ "drill_schedule": "0 4 * * 0" }),
		),
	] {
		let (_, _, body) = post_signals(&f.app, &uri, Some(&cookie), &signals).await;
		assert_contains(&body, "permission");
	}
	let drills: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM restore_drills")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(drills, 0);
	let schedule: Option<String> =
		sqlx::query_scalar("SELECT drill_schedule FROM databases WHERE id = $1")
			.bind(f.database)
			.fetch_one(&db)
			.await
			.unwrap();
	assert_eq!(schedule, None);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn saving_the_drill_schedule_answers_with_the_next_run(db: PgPool) {
	let f = Fixture::new(&db).await;
	let body = f
		.post(
			&format!("{}/drills/schedule", f.base()),
			&json!({ "drill_schedule": "0 4 * * 0" }),
		)
		.await;
	assert_contains(&body, "id=\"drill-next\"");
	assert_contains(&body, "Next drill");
	assert_contains(&body, "Drill schedule saved");
	let (_, page) = f.get(&f.base()).await;
	assert_contains(&page, "&quot;drill_schedule&quot;:&quot;0 4 * * 0&quot;");
}
