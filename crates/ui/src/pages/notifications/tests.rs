//! Notifications pages: what they show, what they never show, and who may change them.
//! `cargo test -p podkit-ui --all-features -- notifications`

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login, ui_session,
};
use crate::tests::notifications_ui as db;
use crate::tests::{
	PgPool, app, call, count, db_test, join_as_project_reader, local_server_id, user_id,
};

const SECRET: &str = "UI-SECRET-TOKEN-4711";
const PASSWORD: &str = "hunter2-smtp-secret";

async fn register(app: &axum::Router, name: &str) {
	let creds = json!({
		"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
	});
	let (s, _) = call(app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(s.is_success());
}

/// alice owns a team and bob joins it as a plain member. Returns (alice, bob, team).
async fn team_with_member(app: &axum::Router, pool: &PgPool) -> (String, String, i64) {
	let (alice, team) = ui_session(app, "alice").await;
	register(app, "bob").await;
	db::add_member(pool, team, "bob").await;
	(alice, ui_login(app, "bob").await, team)
}

fn page(team: i64) -> String {
	format!("/teams/{team}/settings/notifications")
}

fn discord(name: &str, url: &str) -> Value {
	json!({ "ch_name": name, "ch_kind": "discord", "ch_url": url })
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_page_adds_a_channel_and_never_renders_its_secret(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let url = format!("https://discord.example/api/webhooks/1/{SECRET}");

	let (s, _, body) = get_html(&app, &page(team), Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "No channels yet.");
	assert_contains(&body, "Add a channel");
	let main = page_main(&body);
	assert_eq!(main.matches("<h1").count(), 1);
	for section in [
		"Channels",
		"Email server",
		"Rules",
		"Metric alerts",
		"Noise control",
		"Delivery log",
	] {
		assert_contains(main, section);
	}

	let (s, _, body) = post_signals(
		&app,
		&format!("{}/channels", page(team)),
		None,
		&discord("x", &url),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "signed out: sent to the sign-in page");
	assert_contains(&body, "/login");
	assert_eq!(count(&pool, "notification_channels").await, 0);

	let (s, _, body) = post_signals(
		&app,
		&format!("{}/channels", page(team)),
		Some(&alice),
		&discord("<b>Team chat</b>", &url),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, SECRET);
	assert_eq!(count(&pool, "notification_channels").await, 1);
	assert!(!String::from_utf8_lossy(&db::channel_secret(&pool).await).contains(SECRET));

	let (s, _, body) = get_html(&app, &page(team), Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, SECRET);
	assert_not_contains(&body, "webhooks/1");
	let main = page_main(&body);
	assert_contains(main, "&lt;b&gt;Team chat&lt;/b&gt;");
	assert_not_contains(main, "<b>Team chat</b>");
	assert_contains(main, "discord.example");
	assert_contains(main, "Send test");
	assert_eq!(main.matches("<h1").count(), 1);

	// The same name again is refused with a message, and nothing is added.
	let (s, _, body) = post_signals(
		&app,
		&format!("{}/channels", page(team)),
		Some(&alice),
		&discord("<b>Team chat</b>", &url),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "already exists");
	assert_not_contains(&body, SECRET);
	assert_eq!(count(&pool, "notification_channels").await, 1);

	// A webhook with no signing secret, or a kind that does not exist, is refused too.
	for bad in [
		json!({ "ch_name": "hook", "ch_kind": "webhook", "ch_url": "https://example.com/in", "ch_signing": "short" }),
		json!({ "ch_name": "what", "ch_kind": "carrier-pigeon" }),
		json!({ "ch_name": "web", "ch_kind": "discord", "ch_url": "ftp://example.com/x" }),
	] {
		let (s, _, body) = post_signals(
			&app,
			&format!("{}/channels", page(team)),
			Some(&alice),
			&bad,
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "_ch_error");
	}
	assert_eq!(count(&pool, "notification_channels").await, 1);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_email_server_password_is_stored_sealed_and_never_shown(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let smtp = json!({
		"smtp_host": "smtp.example.com", "smtp_port": "587", "smtp_security": "starttls",
		"smtp_user": "mailer", "smtp_password": PASSWORD, "smtp_from": "podkit@example.com",
	});
	let (s, _, body) =
		post_signals(&app, &format!("{}/smtp", page(team)), Some(&alice), &smtp).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, PASSWORD);
	assert!(!String::from_utf8_lossy(&db::smtp_password(&pool).await).contains(PASSWORD));

	let (s, _, body) = get_html(&app, &page(team), Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&body, PASSWORD);
	assert_contains(page_main(&body), "smtp.example.com");
	assert_contains(page_main(&body), "Leave empty to keep the stored one");

	// A bad port is refused with a message.
	let mut bad = smtp.clone();
	bad["smtp_port"] = json!("99999");
	let (_, _, body) =
		post_signals(&app, &format!("{}/smtp", page(team)), Some(&alice), &bad).await;
	assert_contains(&body, "_smtp_error");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_member_sees_but_cannot_change_and_forged_posts_are_refused(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, bob, team) = team_with_member(&app, &pool).await;
	let base = page(team);
	let (s, _, _) = post_signals(
		&app,
		&format!("{base}/channels"),
		Some(&alice),
		&discord("chat", "https://discord.example/hook/abc"),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let id = db::channel_id(&pool).await;

	// Bob may read: he sees the channel but none of the controls.
	let (s, _, body) = get_html(&app, &base, Some(&bob)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_contains(main, "chat");
	for control in [
		"Add a channel",
		"Send test",
		"delete-note-dialog",
		"Add a rule",
		"Add a metric alert",
		"Turn off",
	] {
		assert_not_contains(main, control);
	}

	// Forged posts change nothing.
	let forged: Vec<(String, Value)> = vec![
		(
			format!("{base}/channels"),
			discord("evil", "https://example.com/x"),
		),
		(format!("{base}/channels/{id}/disable"), json!({})),
		(format!("{base}/channels/{id}/delete"), json!({})),
		(format!("{base}/channels/{id}/test"), json!({})),
		(
			format!("{base}/smtp"),
			json!({ "smtp_host": "evil.example", "smtp_port": "25", "smtp_security": "none", "smtp_from": "a@b.c" }),
		),
		(
			format!("{base}/noise"),
			json!({ "nz_dedup": "1", "nz_quiet": true, "nz_start": "00:00", "nz_end": "12:00", "nz_offset": "0" }),
		),
		(
			format!("{base}/rules"),
			json!({ "rule_name": "evil", "ev_deploy_failed": true, format!("rch_{id}"): true }),
		),
		(
			format!("{base}/alerts"),
			json!({ "al_name": "evil", "al_target": "server:1", "al_metric": "cpu", "al_threshold": "50", "al_minutes": "5" }),
		),
	];
	for (path, signals) in &forged {
		let (_, _, _) = post_signals(&app, path, Some(&bob), signals).await;
	}
	assert_eq!(count(&pool, "notification_channels").await, 1);
	assert!(db::channel_enabled(&pool).await);
	assert_eq!(count(&pool, "notification_rules").await, 0);
	assert_eq!(count(&pool, "metric_alerts").await, 0);
	assert_eq!(count(&pool, "notification_smtp").await, 0);
	assert!(db::noise(&pool, team).await.is_none());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn another_teams_page_and_ids_are_not_found(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let (mallory, mallory_team) = ui_session(&app, "mallory").await;
	let (s, _, _) = post_signals(
		&app,
		&format!("{}/channels", page(team)),
		Some(&alice),
		&discord("chat", "https://discord.example/hook/abc"),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let id = db::channel_id(&pool).await;

	let (s, _, body) = get_html(&app, &page(team), Some(&mallory)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&body, "discord.example");
	// Her own team's page does not know the channel either, whatever path it is asked on.
	for path in [
		format!("{}/channels/{id}/delete", page(team)),
		format!("{}/channels/{id}/disable", page(team)),
		format!("{}/channels/{id}/test", page(team)),
		format!("{}/channels/{id}/delete", page(mallory_team)),
		format!("{}/channels/{id}/disable", page(mallory_team)),
		format!("{}/channels/{id}/test", page(mallory_team)),
	] {
		let (_, _, _) = post_signals(&app, &path, Some(&mallory), &json!({})).await;
	}
	assert_eq!(count(&pool, "notification_channels").await, 1);
	assert!(db::channel_enabled(&pool).await);
	let (_, _, body) = get_html(&app, &page(mallory_team), Some(&mallory)).await;
	assert_not_contains(&body, "discord.example");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_role_without_notification_rights_has_no_page_and_no_nav_entry(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	register(&app, "viewer").await;
	let viewer_id = user_id(&pool, "viewer").await;
	join_as_project_reader(&pool, team, viewer_id, 9100).await;
	let viewer = ui_login(&app, "viewer").await;

	let (s, _, body) = get_html(&app, &page(team), Some(&viewer)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_not_contains(&body, "Add a channel");
	// Everyone in the team has the bell, though; owners have the settings entry.
	let (s, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&viewer)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "ui-bell");
	assert_not_contains(&body, &page(team));
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/settings"), Some(&alice)).await;
	assert_contains(&body, &page(team));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn rules_alerts_and_noise_control_are_created_toggled_and_deleted(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let base = page(team);
	let (_, _, _) = post_signals(
		&app,
		&format!("{base}/channels"),
		Some(&alice),
		&discord("chat", "https://discord.example/hook/abc"),
	)
	.await;
	let channel = db::channel_id(&pool).await;

	// A rule needs an event and a channel; a message says which is missing.
	let (_, _, body) = post_signals(
		&app,
		&format!("{base}/rules"),
		Some(&alice),
		&json!({ "rule_name": "r", "rule_min": "info", "rule_scope": "", "ev_deploy_failed": false }),
	)
	.await;
	assert_contains(&body, "at least one event");
	let (_, _, body) = post_signals(
		&app,
		&format!("{base}/rules"),
		Some(&alice),
		&json!({ "rule_name": "r", "rule_min": "info", "rule_scope": "", "ev_deploy_failed": true }),
	)
	.await;
	assert_contains(&body, "at least one channel");
	assert_eq!(count(&pool, "notification_rules").await, 0);

	let mut rule = json!({
		"rule_name": "<i>Failures</i>", "rule_min": "warning", "rule_scope": "",
		"ev_deploy_failed": true, "ev_backup_failed": true,
	});
	rule[format!("rch_{channel}")] = json!(true);
	let (s, _, _) = post_signals(&app, &format!("{base}/rules"), Some(&alice), &rule).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&pool, "notification_rules").await, 1);
	let rule_id = db::rule_id(&pool).await;
	let (_, _, body) = get_html(&app, &base, Some(&alice)).await;
	let main = page_main(&body);
	assert_contains(main, "&lt;i&gt;Failures&lt;/i&gt;");
	assert_not_contains(main, "<i>Failures</i>");
	assert_contains(main, "Deploy failed, Backup failed");
	assert_contains(main, "Warning and up");

	// Scope that is not a project of the team is not found.
	let mut bad = rule.clone();
	bad["rule_name"] = json!("elsewhere");
	bad["rule_scope"] = json!("p:12345");
	let (_, _, body) = post_signals(&app, &format!("{base}/rules"), Some(&alice), &bad).await;
	assert_contains(&body, "_rule_error");
	assert_eq!(count(&pool, "notification_rules").await, 1);

	post_signals(
		&app,
		&format!("{base}/rules/{rule_id}/disable"),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert!(!db::rule_enabled(&pool).await);
	post_signals(
		&app,
		&format!("{base}/rules/{rule_id}/enable"),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert!(db::rule_enabled(&pool).await);

	// A metric alert on the team's own server.
	let server = local_server_id(&pool, team).await;
	let alert = json!({
		"al_name": "Disk", "al_target": format!("server:{server}"), "al_metric": "disk",
		"al_threshold": "90", "al_minutes": "10", "al_severity": "critical",
	});
	let (s, _, _) = post_signals(&app, &format!("{base}/alerts"), Some(&alice), &alert).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(count(&pool, "metric_alerts").await, 1);
	let alert_id = db::alert_id(&pool).await;
	let (_, _, body) = get_html(&app, &base, Some(&alice)).await;
	assert_contains(page_main(&body), "Disk over 90% for 10 min");
	let mut over = alert.clone();
	over["al_name"] = json!("Too much");
	over["al_threshold"] = json!("900");
	let (_, _, body) = post_signals(&app, &format!("{base}/alerts"), Some(&alice), &over).await;
	assert_contains(&body, "_al_error");
	assert_eq!(count(&pool, "metric_alerts").await, 1);
	post_signals(
		&app,
		&format!("{base}/alerts/{alert_id}/disable"),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert!(!db::alert_enabled(&pool).await);

	// Noise control round-trips through the page.
	let noise = json!({ "nz_dedup": "15", "nz_quiet": true, "nz_start": "23:00", "nz_end": "06:30", "nz_offset": "2" });
	let (s, _, _) = post_signals(&app, &format!("{base}/noise"), Some(&alice), &noise).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		db::noise(&pool, team).await,
		Some((15, true, 23 * 60, 6 * 60 + 30, 120))
	);
	let (_, _, body) = get_html(&app, &base, Some(&alice)).await;
	assert_contains(&body, "&quot;nz_start&quot;:&quot;23:00&quot;");
	let mut bad_noise = noise.clone();
	bad_noise["nz_dedup"] = json!("0");
	let (_, _, body) = post_signals(&app, &format!("{base}/noise"), Some(&alice), &bad_noise).await;
	assert_contains(&body, "_nz_error");
	assert_eq!(db::noise(&pool, team).await.unwrap().0, 15);

	// Deleting goes through the same confirm dialog as everything else.
	post_signals(
		&app,
		&format!("{base}/alerts/{alert_id}/delete"),
		Some(&alice),
		&json!({}),
	)
	.await;
	post_signals(
		&app,
		&format!("{base}/rules/{rule_id}/delete"),
		Some(&alice),
		&json!({}),
	)
	.await;
	post_signals(
		&app,
		&format!("{base}/channels/{channel}/delete"),
		Some(&alice),
		&json!({}),
	)
	.await;
	for table in [
		"metric_alerts",
		"notification_rules",
		"notification_channels",
	] {
		assert_eq!(count(&pool, table).await, 0, "{table}");
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn send_test_answers_on_the_card_without_repeating_the_address(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let base = page(team);
	let good = db::ok_server().await;
	let (_, _, _) = post_signals(
		&app,
		&format!("{base}/channels"),
		Some(&alice),
		&discord("good", &format!("{good}/api/webhooks/1/{SECRET}")),
	)
	.await;
	let id = db::channel_id(&pool).await;
	let (s, _, body) = post_signals(
		&app,
		&format!("{base}/channels/{id}/test"),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "Last test: sent.");
	assert_not_contains(&body, SECRET);
	let (_, _, page_body) = get_html(&app, &base, Some(&alice)).await;
	assert_contains(page_main(&page_body), "Last test: sent.");

	let (_, _, _) = post_signals(
		&app,
		&format!("{base}/channels"),
		Some(&alice),
		&discord("dead", &format!("http://127.0.0.1:1/hook/{SECRET}")),
	)
	.await;
	let dead = db::channel_named(&pool, "dead").await;
	let (_, _, body) = post_signals(
		&app,
		&format!("{base}/channels/{dead}/test"),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_contains(&body, "Last test failed");
	assert_not_contains(&body, SECRET);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_bell_counts_unread_and_the_history_marks_them_read(pool: PgPool) {
	let app = app(pool.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	register(&app, "mallory").await;
	let mallory = ui_login(&app, "mallory").await;
	let history = format!("/teams/{team}/notifications");

	let (_, _, body) = get_html(&app, &history, Some(&alice)).await;
	assert_contains(page_main(&body), "Nothing yet.");
	assert_eq!(page_main(&body).matches("<h1").count(), 1);
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&alice)).await;
	assert_not_contains(&body, "ui-bell-count");
	assert_contains(&body, "aria-label=\"Notifications\"");

	let first = db::seed_event(&pool, team, "Deploy of <script>alert(1)</script> failed", 1).await;
	db::seed_event(&pool, team, "Server nas is unreachable", 2).await;

	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&alice)).await;
	assert_contains(&body, "aria-label=\"Notifications, 2 unread\"");
	assert_contains(&body, "ui-bell-count");

	let (s, _, body) = get_html(&app, &history, Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	let main = page_main(&body);
	assert_eq!(main.matches("<h1").count(), 1);
	assert_contains(
		main,
		"Deploy of &lt;script&gt;alert(1)&lt;/script&gt; failed",
	);
	assert_not_contains(main, "<script>alert(1)");
	assert_contains(main, "it broke &lt;b&gt;badly&lt;/b&gt;");
	assert_contains(main, "Mark all as read");
	assert_contains(main, "Critical");

	// Marking one read, then everything.
	let (s, _, _) = post_signals(
		&app,
		&format!("{history}/{first}/read"),
		Some(&alice),
		&json!({}),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&alice)).await;
	assert_contains(&body, "aria-label=\"Notifications, 1 unread\"");
	let (_, _, _) = post_signals(
		&app,
		&format!("{history}/read-all"),
		Some(&alice),
		&json!({}),
	)
	.await;
	let (_, _, body) = get_html(&app, &history, Some(&alice)).await;
	assert_not_contains(page_main(&body), "Mark all as read");
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&alice)).await;
	assert_not_contains(&body, "ui-bell-count");

	// Someone outside the team sees nothing of it, and cannot mark its events.
	let (s, _, body) = get_html(&app, &history, Some(&mallory)).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_not_contains(&body, "unreachable");
	let (_, _, _) = post_signals(
		&app,
		&format!("{history}/{first}/read"),
		Some(&mallory),
		&json!({}),
	)
	.await;
	assert_eq!(db::reads_of(&pool, "mallory").await, 0);
}
