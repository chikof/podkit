//! Volume backups: the archive key and part plan, retention, the restore state machine,
//! the consistency option (the owner is stopped and started around the read), tenancy,
//! permissions, the pages and that no S3 secret is ever rendered.
//!
//! The agent is a fake that records what it is asked, and so is the object store, which
//! answers the three multipart requests the control plane makes itself. One live test
//! (ignored) runs the real agent against Podman and a Garage container.
//!
//! `cargo test -p podkit-core -p podkit-ui --all-features -- volume_backup`
//!
//! The tests of `podkit_core::volume_backups` that need the pages: they sign in through the
//! web UI or read what it renders, so they run against the whole router. The rest are beside
//! the code, in `podkit_core::volume_backups::tests`.

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};
use crate::tests::{call, join_as_project_reader, server_id, state, user_id, user_with_team};
use podkit_core::jobs::{self, JobKind};
use podkit_core::tests::volume_backups::{Fx, SECRET, backed_up};

/// The shared [`Fx`] over the whole router: these tests read the pages.
async fn fx(db: PgPool) -> Fx {
	let state = state(db.clone());
	let app = crate::routes(state.clone()).await.unwrap();
	Fx::over(db, state, app, true).await
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn volume_backup_a_reader_cannot_forge_any_action(db: PgPool) {
	let f = fx(db).await;
	let backup = backed_up(&f).await;
	user_with_team(&f.app, "viewer").await;
	let viewer_id = user_id(&f.db, "viewer").await;
	join_as_project_reader(&f.db, f.team, viewer_id, 9100).await;
	let (_, login) = call(
		&f.app,
		"POST",
		"/api/v1/auth/login",
		None,
		Some(json!({ "email": "viewer@example.com", "password": "correct horse battery" })),
	)
	.await;
	let viewer = login["token"].as_str().unwrap().to_owned();

	for (method, rest, body) in [
		("GET", String::new(), None),
		("POST", String::new(), None),
		("PUT", "/settings".to_owned(), Some(json!({ "keep": 1 }))),
		("DELETE", format!("/{backup}"), None),
		(
			"POST",
			format!("/{backup}/restore"),
			Some(json!({ "mode": "same", "confirm": "/data" })),
		),
	] {
		let (s, _) = call(&f.app, method, &f.api(&rest), Some(&viewer), body).await;
		assert!(
			s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
			"{method} {rest}: {s}"
		);
	}
	assert_eq!(f.backup_count().await, 1);
	assert!(f.agent.calls().is_empty());

	// The same through the page's own routes.
	let cookie = ui_login(&f.app, "viewer").await;
	let ui = f.ui(&format!("/{}/backups", f.volume));
	for (path, signals) in [
		(ui.clone(), json!({})),
		(format!("{ui}/settings"), json!({ "vb_keep": "1" })),
		(format!("{ui}/{backup}/delete"), json!({})),
		(
			format!("{ui}/{backup}/restore"),
			json!({ "vb_confirm": "/data", "vb_mode": "same" }),
		),
	] {
		let (_, _, body) = post_signals(&f.app, &path, Some(&cookie), &signals).await;
		assert!(
			!body.contains("Restore queued") && !body.contains("Backup deleted"),
			"{path}: {body}"
		);
	}
	assert_eq!(f.backup_count().await, 1);
	assert!(f.agent.calls().is_empty());
}

// --- the pages and secrets ------------------------------------------------------------------

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn volume_backup_pages_name_the_destination_and_never_show_its_secret(db: PgPool) {
	let f = fx(db).await;
	backed_up(&f).await;
	let cookie = ui_login(&f.app, "alice").await;

	let (s, _, body) = get_html(&f.app, &f.ui(""), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "Backups \u{b7} /data");
	assert_contains(main, "garage");
	assert_contains(main, "Back up now");
	assert_contains(main, "Restore");
	assert_contains(main, "abababababab");
	assert_contains(main, "3 items");
	for needle in [SECRET, "AKIDEXAMPLE"] {
		assert_not_contains(&body, needle);
	}
	assert_eq!(body.matches("<h1").count(), 1, "one h1 per page");

	// The JSON side tells the same: a destination by id, never its keys.
	let (_, settings) = f.call("GET", "/settings", None).await;
	let (_, listed) = f.call("GET", "", None).await;
	for body in [settings.to_string(), listed.to_string()] {
		assert_not_contains(&body, SECRET);
		assert_not_contains(&body, "AKIDEXAMPLE");
	}
	// The stored secret is encrypted.
	let stored: Vec<u8> = sqlx::query_scalar("SELECT secret_access_key FROM backup_targets")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&stored).contains(SECRET));
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn volume_backup_page_escapes_the_volume_path(db: PgPool) {
	let f = fx(db).await;
	let hostile = "/x<img src=x onerror=alert(1)>";
	sqlx::query("UPDATE volumes SET mount_path = $2 WHERE id = $1")
		.bind(f.volume)
		.bind(hostile)
		.execute(&f.db)
		.await
		.unwrap();
	let cookie = ui_login(&f.app, "alice").await;

	let (s, _, body) = get_html(&f.app, &f.ui(""), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, hostile);
	assert_contains(&body, "&lt;img src=x onerror=alert(1)&gt;");
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn volume_backup_the_page_starts_a_backup_and_checks_the_typed_name(db: PgPool) {
	let f = fx(db).await;
	f.configure(json!({})).await;
	let cookie = ui_login(&f.app, "alice").await;
	let ui = f.ui(&format!("/{}/backups", f.volume));

	// Back up now: the card comes back, and the backup waits for its job.
	let (s, _, body) = post_signals(&f.app, &ui, Some(&cookie), &json!({})).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, &format!("id=\"vb-{}\"", f.volume));
	assert_eq!(f.backup_count().await, 1);
	let backup: i64 = sqlx::query_scalar("SELECT id FROM volume_backups")
		.fetch_one(&f.db)
		.await
		.unwrap();
	let job = jobs::latest_for(&f.db, JobKind::VolumeBackup, backup)
		.await
		.unwrap()
		.unwrap();
	f.run(job).await.unwrap();
	f.agent.clear();
	let (_, _, body) = post_signals(
		&f.app,
		&format!("{ui}/{backup}/restore"),
		Some(&cookie),
		&json!({ "vb_confirm": "wrong", "vb_mode": "same" }),
	)
	.await;
	assert_contains(&body, "doesn't match");
	let restores: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volume_restores")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert_eq!(restores, 0);

	let (_, _, body) = post_signals(
		&f.app,
		&format!("{ui}/{backup}/restore"),
		Some(&cookie),
		&json!({ "vb_confirm": "/data", "vb_mode": "same" }),
	)
	.await;
	assert_contains(&body, "Restore queued");
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn volume_backup_the_stack_volumes_tab_lists_the_compose_volumes(db: PgPool) {
	let f = fx(db).await;
	let (s, stack) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{}/projects/{}/stacks", f.team, f.project),
		Some(&f.token),
		Some(json!({
			"name": "blog", "server_id": server_id(&f.db, f.team).await, "env": {},
			"compose_yaml": "services:\n  web:\n    image: ghcr.io/acme/web:1\n    volumes:\n      - uploads:/srv\nvolumes:\n  uploads:\n",
		})),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{stack}");
	let stack = stack["id"].as_i64().unwrap();
	let cookie = ui_login(&f.app, "alice").await;

	let (s, _, body) = get_html(
		&f.app,
		&format!(
			"/teams/{}/projects/{}/stacks/{stack}/volumes",
			f.team, f.project
		),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(page_main(&body), "Backups \u{b7} uploads");

	// A policy is made on first use; without a destination the backup says so.
	let ui = format!(
		"/teams/{}/projects/{}/stacks/{stack}/volumes/uploads/backups",
		f.team, f.project
	);
	let (_, _, body) = post_signals(&f.app, &ui, Some(&cookie), &json!({})).await;
	assert_contains(&body, "configure a backup destination first");
	assert_eq!(f.backup_count().await, 0);
}
