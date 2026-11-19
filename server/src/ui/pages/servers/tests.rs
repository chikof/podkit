//! Server page tests. The first block carries over, one for one, the tests of the old
//! `ui/servers.rs` (same behaviour, new markup); the second covers the cards, the stepped
//! dialog and the Monitoring tab.
//!
//! | old test | here |
//! |---|---|
//! | `the_list_shows_the_local_server_and_the_form` | same name |
//! | `adding_a_server_bootstraps_it_and_never_echoes_the_key` | same name |
//! | `bad_input_shows_a_message_and_creates_nothing` | same name |
//! | `the_detail_page_streams_the_bootstrap_log` | same name |
//! | `the_log_of_a_server_with_no_job_ends_at_once` | same name |
//! | `reset_host_key_sends_the_flag_and_forgets_the_key_a_plain_rerun_keeps_it` | same name |
//! | `the_local_server_has_no_bootstrap_or_delete_and_refuses_both` | same name |
//! | `deleting_needs_the_exact_name_and_shows_refusals_inline` | same name |
//! | `a_name_with_markup_is_escaped_everywhere` | same name, plus the Monitoring pages |
//! | `another_teams_server_is_the_404_page_and_cannot_be_touched` | same name, plus Monitoring |
//! | `a_read_only_member_sees_no_form_or_actions_and_forged_posts_fail` | same name |

use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use proto::{Cpu, DiskStats, Gpu, Hardware, HostStats, Memory, Stats};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::ids::ServerId;
use crate::jobs::{JobKind, latest_for};
use crate::state::AppState;
use crate::tests::{
	PgPool, call, count, db_test, project_id, seed_application_on, server_id, set_hardware, state,
	user_with_team,
};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login, ui_session,
};

const KEY: &str = "[REDACTED PRIVATE KEY]\n";
const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJchJVs7z1tmicd4SpMN5UdQGOeEF7c+3zkUwGLv44BI";
const ED25519_FP: &str = "SHA256:ywBw9jYLna1cd5Li260999IYfpxB/BrS/G6GhT1Xkwg";

async fn app_and_state(db: PgPool) -> (Router, AppState) {
	let state = state(db);
	let app = crate::routes::routes(state.clone()).await.unwrap();
	(app, state)
}

fn new_server(name: &str) -> Value {
	json!({
		"name": name, "hostname": "203.0.113.10", "ssh_port": 22, "ssh_user": "root",
		"ssh_private_key": KEY, "podman_socket_path": "%t/podman/podman.sock",
	})
}

/// Adds a server through the UI and returns its id.
async fn add(app: &Router, cookie: &str, team: i64, name: &str) -> i64 {
	let (s, _, body) = post_signals(
		app,
		&format!("/teams/{team}/servers"),
		Some(cookie),
		&new_server(name),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, &format!("/teams/{team}/servers/"));
	add_id_from(&body, team)
}

/// The id and name of the team's local server.
async fn local_server(db: &PgPool, team: i64) -> (i64, String) {
	let server = crate::servers::list_by_team(db, crate::ids::TeamId(team))
		.await
		.unwrap()
		.into_iter()
		.find(|s| s.is_local)
		.unwrap();
	(server.id.0, server.name)
}

fn add_id_from(body: &str, team: i64) -> i64 {
	body.split(&format!("/teams/{team}/servers/"))
		.nth(1)
		.unwrap()
		.split(|c: char| !c.is_ascii_digit())
		.next()
		.unwrap()
		.parse()
		.unwrap()
}

async fn ui_token(app: &Router) -> String {
	let creds = json!({ "email": "alice@example.com", "password": "correct horse battery" });
	let (_, login) = call(app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
	login["token"].as_str().unwrap().to_owned()
}

/// `n` samples of `server`, one every 30 s up to now: CPU rises 10, 11, …; network
/// counters grow by 3000 bytes received per sample.
async fn sample(db: &PgPool, server: i64, n: i64, gpu: bool) {
	let now = OffsetDateTime::now_utc();
	for i in 0..n {
		let stats = Stats {
			host: HostStats {
				cpu_percent: Some(10.0 + i as f64),
				mem_used: 4 * 1024 * 1024 * 1024,
				mem_total: 16 * 1024 * 1024 * 1024,
				disks: vec![DiskStats {
					path: "/data".to_owned(),
					used: 100 * 1024 * 1024 * 1024,
					free: 400 * 1024 * 1024 * 1024,
					total: 500 * 1024 * 1024 * 1024,
				}],
				net_rx: 1_000_000 + 3000 * i.cast_unsigned(),
				net_tx: 500_000 + 1000 * i.cast_unsigned(),
				gpu_util: gpu.then_some(55.0),
				vram_used: gpu.then_some(2 * 1024 * 1024 * 1024),
			},
			containers: Vec::new(),
		};
		crate::metrics::record(
			db,
			ServerId(server),
			now - time::Duration::seconds(30 * (n - 1 - i)),
			&stats,
		)
		.await
		.unwrap();
	}
}

fn hardware(gpu: bool) -> Hardware {
	Hardware {
		cpu: Cpu {
			model: "Test CPU".to_owned(),
			threads: 16,
		},
		memory: Memory {
			total_mb: 32 * 1024,
			available_mb: 20 * 1024,
		},
		disks: vec![proto::Disk {
			path: "/data".to_owned(),
			free_mb: 400 * 1024,
			total_mb: 500 * 1024,
		}],
		gpus: if gpu {
			vec![Gpu {
				vendor: "nvidia".to_owned(),
				name: "RTX 4090".to_owned(),
				vram_mb: 24 * 1024,
				vram_used_mb: Some(1024),
			}]
		} else {
			Vec::new()
		},
		cdi: proto::Cdi::default(),
	}
}

// ---- Carried over from the old pages ---------------------------------------------------

#[db_test]
async fn the_list_shows_the_local_server_and_the_form(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;

	let (s, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "<title>Servers");
	assert_contains(&body, "Local");
	assert_contains(&body, "Active");
	assert_contains(&body, "Add a server");
	assert_contains(&body, "SSH private key");
	assert_contains(&body, "%t/podman/podman.sock");
	assert_contains(&body, "systemd");
	assert_contains(&body, "ssh_port: 22");
	assert_not_contains(&body, "BEGIN");
}

#[db_test]
async fn adding_a_server_bootstraps_it_and_never_echoes_the_key(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;

	let (s, _, created) = post_signals(
		&app,
		&format!("/teams/{team}/servers"),
		Some(&cookie),
		&new_server("edge-1"),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&created, "BEGIN");
	let id = add_id_from(&created, team);
	assert_eq!(count(&db, "jobs").await, 1);
	// The dialog moves on to its bootstrap step, with the live log, and drops the key.
	assert_contains(&created, "\"_step\":3");
	assert_contains(&created, "\"ssh_private_key\":\"\"");
	assert_contains(&created, &format!("@get('/teams/{team}/servers/{id}/log')"));
	assert_contains(&created, "id=\"bootstrap-log\"");

	// The port may arrive as text; a second server proves it.
	let mut other = new_server("edge-2");
	other["ssh_port"] = json!("2222");
	let (_, _, body) = post_signals(
		&app,
		&format!("/teams/{team}/servers"),
		Some(&cookie),
		&other,
	)
	.await;
	assert_contains(&body, &format!("/teams/{team}/servers/"));
	assert_eq!(count(&db, "jobs").await, 2);

	let (s, _, list) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&list, "edge-1");
	assert_contains(&list, "203.0.113.10:22");
	assert_contains(&list, "203.0.113.10:2222");
	assert_contains(&list, "Pending");
	assert_contains(&list, "/servers/rows");
	assert_not_contains(&list, "BEGIN");

	let (s, _, rows) = get_html(&app, &format!("/teams/{team}/servers/rows"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&rows, "server-list");
	assert_not_contains(&rows, "BEGIN");

	let (s, _, page) = get_html(&app, &format!("/teams/{team}/servers/{id}"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "edge-1");
	assert_contains(&page, "Bootstrap queued");
	assert_not_contains(&page, "BEGIN");
	let (_, _, status) = get_html(
		&app,
		&format!("/teams/{team}/servers/{id}/status"),
		Some(&cookie),
	)
	.await;
	assert_not_contains(&status, "BEGIN");

	// Stored encrypted, and the JSON API agrees there is no key to read.
	let token = ui_token(&app).await;
	let (_, json) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		Some(&token),
		None,
	)
	.await;
	assert_not_contains(&json.to_string(), "BEGIN");
	assert_not_contains(&json.to_string(), "ssh_private_key");
}

#[db_test]
async fn bad_input_shows_a_message_and_creates_nothing(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let uri = format!("/teams/{team}/servers");

	let cases = [
		("name", json!(""), "Name must be"),
		("hostname", json!("not a host"), "invalid hostname"),
		("ssh_user", json!("Root User"), "invalid ssh_user"),
		("ssh_port", json!(70000), "SSH port"),
		("ssh_port", json!("abc"), "SSH port"),
		(
			"podman_socket_path",
			json!("relative/path"),
			"podman_socket_path",
		),
		("ssh_private_key", json!("  "), "private key is required"),
	];
	for (field, value, message) in cases {
		let mut body = new_server("edge");
		body[field] = value;
		let (s, _, out) = post_signals(&app, &uri, Some(&cookie), &body).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&out, message);
		assert_not_contains(&out, "BEGIN");
	}
	assert_eq!(count(&db, "servers").await, 1, "only the local server");
	assert_eq!(count(&db, "jobs").await, 0);
}

#[db_test]
async fn the_detail_page_streams_the_bootstrap_log(db: PgPool) {
	let (app, state) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let id = add(&app, &cookie, team, "edge-1").await;
	let job = latest_for(&db, JobKind::ServerBootstrap, id)
		.await
		.unwrap()
		.unwrap();

	let page = format!("/teams/{team}/servers/{id}");
	let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_contains(&body, "Bootstrap log");
	assert_contains(&body, &format!("@get('{page}/log')"));
	assert_contains(&body, "id=\"bootstrap-log\"");

	let mut log = state.logs.open(job.id).await.unwrap();
	log.push(proto::Stream::System, "connecting to 203.0.113.10")
		.await;
	log.push(proto::Stream::Stdout, "<script>alert(1)</script>")
		.await;
	let reader = {
		let (app, cookie, uri) = (app.clone(), cookie.clone(), format!("{page}/log"));
		tokio::spawn(async move { get_html(&app, &uri, Some(&cookie)).await })
	};
	tokio::time::sleep(Duration::from_millis(200)).await;
	log.push(proto::Stream::Stdout, "agent installed").await;
	log.finish().await;

	let (s, _, stream) = tokio::time::timeout(Duration::from_secs(5), reader)
		.await
		.expect("log stream ends with the job")
		.unwrap();
	assert_eq!(s, StatusCode::OK);
	assert_contains(&stream, "connecting to 203.0.113.10");
	assert_contains(&stream, "agent installed");
	assert_contains(&stream, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_not_contains(&stream, "<script>");
	assert_contains(&stream, "#bootstrap-log");
	assert_contains(&stream, "\"_live\":false");
}

#[db_test]
async fn the_log_of_a_server_with_no_job_ends_at_once(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (local, _) = local_server(&db, team).await;

	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/servers/{local}/log"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "\"_live\":false");
}

#[db_test]
async fn reset_host_key_sends_the_flag_and_forgets_the_key_a_plain_rerun_keeps_it(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let id = add(&app, &cookie, team, "edge-1").await;
	let page = format!("/teams/{team}/servers/{id}");
	let line = format!("203.0.113.10 ssh-ed25519 {ED25519}");
	crate::servers::set_host_key(&db, ServerId(id), Some(&line))
		.await
		.unwrap();

	let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_contains(&body, ED25519_FP);
	assert_contains(
		&body,
		&format!("@post('{page}/bootstrap?reset_host_key=true')"),
	);
	assert_contains(&body, &format!("@post('{page}/bootstrap')"));
	assert_contains(&body, "Reset it only when you know the host changed");
	// Behind a confirm: the dialog is opened by a button and the danger action sits in it.
	assert_contains(&body, "commandfor=\"reset-host-key\"");
	let (_, dialog) = body.split_once("<dialog id=\"reset-host-key\"").unwrap();
	let dialog = dialog.split("</dialog>").next().unwrap();
	assert_contains(dialog, "reset_host_key=true");
	assert_contains(dialog, "Cancel");

	let (_, _, out) = post_signals(
		&app,
		&format!("{page}/bootstrap"),
		Some(&cookie),
		&json!({}),
	)
	.await;
	assert_contains(&out, &page);
	let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_contains(&body, ED25519_FP);
	assert_eq!(count(&db, "jobs").await, 2);

	let (_, _, out) = post_signals(
		&app,
		&format!("{page}/bootstrap?reset_host_key=true"),
		Some(&cookie),
		&json!({}),
	)
	.await;
	assert_contains(&out, &page);
	let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_not_contains(&body, ED25519_FP);
	assert_contains(&body, "not pinned yet");
	assert_eq!(count(&db, "jobs").await, 3);
}

#[db_test]
async fn the_local_server_has_no_bootstrap_or_delete_and_refuses_both(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (local, local_name) = local_server(&db, team).await;
	let page = format!("/teams/{team}/servers/{local}");

	let (s, _, body) = get_html(&app, &page, Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "Local");
	assert_not_contains(&body, "Re-run bootstrap");
	assert_not_contains(&body, "Delete server");
	assert_not_contains(&body, "reset-host-key");

	let (_, _, out) = post_signals(
		&app,
		&format!("{page}/bootstrap"),
		Some(&cookie),
		&json!({}),
	)
	.await;
	assert_contains(&out, "managed by the operator");
	let (_, _, out) = post_signals(
		&app,
		&format!("{page}/delete"),
		Some(&cookie),
		&json!({ "confirm": local_name }),
	)
	.await;
	assert_contains(&out, "cannot be deleted");
	assert_eq!(count(&db, "servers").await, 1);
}

#[db_test]
async fn deleting_needs_the_exact_name_and_shows_refusals_inline(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let id = add(&app, &cookie, team, "edge-1").await;
	let uri = format!("/teams/{team}/servers/{id}/delete");

	// The delete is behind a confirm dialog that names the server to type.
	let (_, _, page) = get_html(&app, &format!("/teams/{team}/servers/{id}"), Some(&cookie)).await;
	assert_contains(&page, "<dialog id=\"delete-server\"");
	assert_contains(&page, "commandfor=\"delete-server\"");

	let (_, _, body) = post_signals(&app, &uri, Some(&cookie), &json!({ "confirm": "edge" })).await;
	assert_contains(&body, "doesn't match");
	assert_eq!(count(&db, "servers").await, 2);

	let (_, _, body) =
		post_signals(&app, &uri, Some(&cookie), &json!({ "confirm": "edge-1" })).await;
	assert_contains(&body, &format!("/teams/{team}/servers"));
	assert_eq!(count(&db, "servers").await, 1);

	// A server that still hosts an application is refused with a message.
	let id = add(&app, &cookie, team, "edge-2").await;
	post_signals(
		&app,
		&format!("/teams/{team}/projects"),
		Some(&cookie),
		&json!({ "name": "Shop", "slug": "shop" }),
	)
	.await;
	let project = project_id(&db, "shop").await;
	seed_application_on(&db, project, id).await;
	let (_, _, body) = post_signals(
		&app,
		&format!("/teams/{team}/servers/{id}/delete"),
		Some(&cookie),
		&json!({ "confirm": "edge-2" }),
	)
	.await;
	assert_contains(&body, "still runs applications or databases");
	assert_eq!(count(&db, "servers").await, 2);
}

#[db_test]
async fn a_name_with_markup_is_escaped_everywhere(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let name = "<script>alert(1)</script>";
	let id = add(&app, &cookie, team, name).await;
	sample(&db, id, 3, false).await;

	for uri in [
		format!("/teams/{team}/servers"),
		format!("/teams/{team}/servers/rows"),
		format!("/teams/{team}/servers/{id}"),
		format!("/teams/{team}/servers/{id}/monitoring"),
	] {
		let (s, _, body) = get_html(&app, &uri, Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK, "{uri}");
		assert_not_contains(&body, name);
		assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}
}

#[db_test]
async fn another_teams_server_is_the_404_page_and_cannot_be_touched(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (alice, alice_team) = ui_session(&app, "alice").await;
	let (bob, bob_team) = ui_session(&app, "bob").await;
	let secret = add(&app, &alice, alice_team, "secret-box").await;
	sample(&db, secret, 3, false).await;

	for uri in [
		format!("/teams/{alice_team}/servers"),
		format!("/teams/{alice_team}/servers/{secret}"),
		format!("/teams/{alice_team}/servers/{secret}/monitoring"),
		format!("/teams/{bob_team}/servers/{secret}"),
		format!("/teams/{bob_team}/servers/{secret}/monitoring"),
		"/teams/999/servers".to_owned(),
	] {
		let (s, _, body) = get_html(&app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_contains(&body, "Not found");
		assert_not_contains(&body, "secret-box");
		assert_not_contains(&body, "203.0.113.10");
		assert_not_contains(&body, "chart-cpu");
	}

	// The streams and polls answer a Datastar GET: a message, never data.
	for uri in [
		format!("/teams/{bob_team}/servers/{secret}/log"),
		format!("/teams/{bob_team}/servers/{secret}/status"),
		format!("/teams/{bob_team}/servers/{secret}/monitoring/charts"),
		format!("/teams/{bob_team}/servers/{secret}/monitoring/charts?range=7d"),
	] {
		let (_, _, out) = get_html(&app, &uri, Some(&bob)).await;
		assert_contains(&out, "That no longer exists.");
		assert_not_contains(&out, "secret-box");
		assert_not_contains(&out, "chart-cpu");
		assert_not_contains(&out, "_live");
	}
	for uri in [
		format!("/teams/{alice_team}/servers/{secret}/status"),
		format!("/teams/{alice_team}/servers/{secret}/log"),
		format!("/teams/{alice_team}/servers/{secret}/monitoring/charts"),
		format!("/teams/{alice_team}/servers/rows"),
	] {
		let (s, _, out) = get_html(&app, &uri, Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
		assert_not_contains(&out, "secret-box");
		assert_not_contains(&out, "chart-cpu");
	}

	for uri in [
		format!("/teams/{alice_team}/servers"),
		format!("/teams/{bob_team}/servers/{secret}/bootstrap"),
		format!("/teams/{bob_team}/servers/{secret}/bootstrap?reset_host_key=true"),
		format!("/teams/{bob_team}/servers/{secret}/delete"),
	] {
		let mut body = new_server("planted");
		body["confirm"] = json!("secret-box");
		let (_, _, out) = post_signals(&app, &uri, Some(&bob), &body).await;
		assert_contains(&out, "That no longer exists.");
	}
	assert_eq!(count(&db, "servers").await, 3);
	assert_eq!(count(&db, "jobs").await, 1);
}

#[db_test]
async fn a_read_only_member_sees_no_form_or_actions_and_forged_posts_fail(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (owner_token, team) = user_with_team(&app, "alice").await;
	let owner = ui_login(&app, "alice").await;
	let (_, _) = user_with_team(&app, "carol").await;
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(&owner_token),
		Some(json!({ "email": "carol@example.com", "role_id": 2 })),
	)
	.await;
	assert!(s.is_success());
	let carol = ui_login(&app, "carol").await;
	let id = add(&app, &owner, team, "edge-1").await;
	let jobs_before = count(&db, "jobs").await;

	let (s, _, list) = get_html(&app, &format!("/teams/{team}/servers"), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&list, "edge-1");
	assert_not_contains(page_main(&list), "Add a server");
	assert_not_contains(&list, "SSH private key");
	assert_not_contains(&list, "add-server-form");

	let (s, _, page) = get_html(&app, &format!("/teams/{team}/servers/{id}"), Some(&carol)).await;
	assert_eq!(s, StatusCode::OK);
	assert_not_contains(&page, "Re-run bootstrap");
	assert_not_contains(&page, "Delete server");
	assert_not_contains(&page, "reset-host-key");

	// Reading the charts needs only `read server`.
	let (s, _, mon) = get_html(
		&app,
		&format!("/teams/{team}/servers/{id}/monitoring"),
		Some(&carol),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&mon, "id=\"chart-cpu\"");

	let forbidden = "You don't have permission to do that.";
	let (_, _, out) = post_signals(
		&app,
		&format!("/teams/{team}/servers"),
		Some(&carol),
		&new_server("planted"),
	)
	.await;
	assert_contains(&out, forbidden);
	assert_not_contains(&out, "BEGIN");
	for uri in [
		format!("/teams/{team}/servers/{id}/bootstrap"),
		format!("/teams/{team}/servers/{id}/bootstrap?reset_host_key=true"),
		format!("/teams/{team}/servers/{id}/delete"),
	] {
		let (_, _, out) =
			post_signals(&app, &uri, Some(&carol), &json!({ "confirm": "edge-1" })).await;
		assert_contains(&out, forbidden);
	}
	assert_eq!(
		count(&db, "servers").await,
		3,
		"two local servers and edge-1"
	);
	assert_eq!(count(&db, "jobs").await, jobs_before);
}

// ---- The redesign: cards, the stepped dialog, Monitoring ---------------------------------

#[db_test]
async fn the_cards_show_os_arch_hardware_and_sparklines(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (local, _) = local_server(&db, team).await;

	// Before any reading the card says so instead of drawing nothing.
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
	assert_contains(&body, "Hardware not reported yet.");
	assert_contains(&body, "No readings yet.");

	set_hardware(&db, team, &hardware(true)).await;
	sample(&db, local, 8, false).await;
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
	let main = page_main(&body);
	assert_contains(main, "Linux");
	assert_contains(main, "16 threads");
	assert_contains(main, "32 GB RAM");
	assert_contains(main, "500 GB disk");
	assert_contains(main, "RTX 4090 24 GB");
	// CPU 17% is the newest of 10, 11, …, 17; memory 4 of 16 GiB is 25%.
	assert_contains(main, ">17%<");
	assert_contains(main, ">25%<");
	assert!(main.matches("<svg").count() >= 2, "{main}");
	assert_not_contains(main, "<title>");
	// A decorative picture: its numbers are in the text beside it.
	assert_contains(main, "aria-hidden=\"true\" focusable=\"false\"");
	// The whole row is one link to the server (a stretched link).
	assert_contains(main, &format!("href=\"/teams/{team}/servers/{local}\""));
	assert_contains(main, "ui-row-link");
}

#[db_test]
async fn the_cards_refresh_while_visible_and_never_send_the_ssh_key(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
	// Every 30 s, not while the tab is hidden or the dialog is open, and with no signals
	// in the query (the key signal lives on this page).
	assert_contains(
		&body,
		"data-on-interval__duration.30s=\"document.hidden || ",
	);
	assert_contains(&body, "document.querySelector('dialog[open]')");
	assert_contains(&body, "filterSignals: {include: /^$/}");
	// Closing the dialog forgets a half-typed key.
	assert_contains(&body, "$ssh_private_key = ''");
}

#[db_test]
async fn adding_a_server_is_three_steps_in_one_dialog(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (_, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;

	assert_contains(&body, "commandfor=\"add-server\"");
	let (_, dialog) = body.split_once("<dialog id=\"add-server\"").unwrap();
	let dialog = dialog.split("</dialog>").next().unwrap();
	for step in ["Connection", "SSH key", "Bootstrap"] {
		assert_contains(dialog, step);
	}
	for id in ["add-step-1", "add-step-2", "add-server-result"] {
		assert_contains(dialog, &format!("id=\"{id}\""));
	}
	// Each step validates only its own fields; the key's field is write-only.
	assert_contains(dialog, "reportValidity()");
	assert_contains(dialog, "<textarea");
	assert_not_contains(dialog, "BEGIN");
	assert_contains(&body, "ssh_private_key: ''");
	assert_contains(dialog, "Stored encrypted. It is never shown again");
	// Form markup the browser can complete.
	assert_contains(dialog, "autofocus");
	assert_contains(dialog, "novalidate");
	// Back and Next; Esc closes (the native dialog does it).
	assert_contains(dialog, "Back");
	assert_contains(dialog, "Next");
	assert_not_contains(dialog, "role=\"dialog\"");
}

#[db_test]
async fn the_detail_page_has_tabs_and_the_hardware(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let (local, _) = local_server(&db, team).await;
	set_hardware(&db, team, &hardware(false)).await;

	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{team}/servers/{local}"),
		Some(&cookie),
	)
	.await;
	let main = page_main(&body);
	assert_contains(main, "aria-label=\"Server sections\"");
	assert_contains(main, "aria-current=\"page\">Overview");
	assert_contains(main, &format!("/teams/{team}/servers/{local}/monitoring"));
	// The hardware strip: one cell each, with what the agent reported.
	assert_contains(main, "aria-label=\"Hardware\"");
	assert_contains(main, ">Test CPU<");
	assert_contains(main, "16 threads");
	assert_contains(main, ">32 GB<");
	assert_contains(main, "20 GB available");
	assert_contains(main, ">500 GB<");
	assert_contains(main, "400 GB free");
	assert_not_contains(main, "BEGIN");
}

#[db_test]
async fn the_monitoring_tab_draws_every_chart_from_the_samples(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let local = server_id(&db, team).await;
	sample(&db, local, 20, false).await;

	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/servers/{local}/monitoring"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "<title>");
	assert_contains(&body, "aria-current=\"page\">Monitoring");
	for chart in ["cpu", "memory", "disk", "network"] {
		assert_contains(&body, &format!("id=\"chart-{chart}\""));
		// Accessible: the picture is described by its summary, the numbers sit in a table.
		assert_contains(
			&body,
			&format!("aria-describedby=\"chart-{chart}-summary\""),
		);
		assert_contains(&body, &format!("id=\"chart-{chart}-summary\""));
	}
	// No GPU readings: no GPU charts.
	assert_not_contains(&body, "id=\"chart-gpu\"");
	assert_not_contains(&body, "id=\"chart-vram\"");
	// Figures: CPU 10, 11, … 29; memory 4 GB of 16 GB; disk 100 of 500 GB.
	assert_contains(&body, "20 readings. Latest 29%");
	assert_contains(&body, "4 GB");
	assert_contains(&body, "100 GB");
	// Network rates come from the counters: 3000 bytes every 30 s is 100 B/s.
	assert_contains(&body, "100 B/s");
	// Range links, data-table toggle and polling that stops while hidden.
	for r in ["?range=1h", "?range=24h", "?range=7d"] {
		assert_contains(&body, r);
	}
	assert_contains(&body, "aria-pressed=\"false\"");
	assert_contains(&body, "data-show=\"$_tables\"");
	assert_contains(
		&body,
		&format!(
			"data-on-interval__duration.30s=\"document.hidden || @get('/teams/{team}/servers/{local}/monitoring/charts?range=1h')\""
		),
	);
	assert_contains(
		&body,
		"data-on:visibilitychange__window=\"document.hidden || ",
	);
	// The chart region has no script of its own.
	let (_, region) = body.split_once("id=\"metrics-body\"").unwrap();
	let region = region.split("</section>").next().unwrap();
	assert_not_contains(region, "<script");
	assert_not_contains(region, "javascript:");
}

#[db_test]
async fn the_monitoring_tab_adds_gpu_charts_when_there_are_gpu_readings(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let local = server_id(&db, team).await;
	set_hardware(&db, team, &hardware(true)).await;
	sample(&db, local, 5, true).await;

	let (_, _, body) = get_html(
		&app,
		&format!("/teams/{team}/servers/{local}/monitoring"),
		Some(&cookie),
	)
	.await;
	assert_contains(&body, "id=\"chart-gpu\"");
	assert_contains(&body, "id=\"chart-vram\"");
	assert_contains(&body, "Latest 55%");
	// VRAM tops out at the card's 24 GB.
	assert_contains(&body, "24 GB");
}

#[db_test]
async fn the_monitoring_tab_without_samples_says_so(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let local = server_id(&db, team).await;

	let (s, _, body) = get_html(
		&app,
		&format!("/teams/{team}/servers/{local}/monitoring"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "No readings in this range yet");
	assert_not_contains(page_main(&body), "<svg");
}

#[db_test]
async fn the_range_picks_the_window_and_an_unknown_one_means_the_last_hour(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let local = server_id(&db, team).await;
	sample(&db, local, 4, false).await;
	let page = format!("/teams/{team}/servers/{local}/monitoring");

	for (query, current, phrase) in [
		("?range=24h", "24h", "last 24 hours"),
		("?range=7d", "7d", "last 7 days"),
		("?range=1h", "1h", "last hour"),
		("?range=forever", "1h", "last hour"),
		("", "1h", "last hour"),
	] {
		let (s, _, body) = get_html(&app, &format!("{page}{query}"), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK, "{query}");
		assert_contains(&body, &format!("aria-current=\"true\">{current}"));
		assert_contains(&body, &format!("CPU, {phrase}."));
		// The segmented range control, and a refresh that never runs in a hidden tab.
		assert_contains(&body, "class=\"ui-range-link\" aria-current=\"true\"");
		assert_contains(&body, "document.hidden || @get(");
	}
}

#[db_test]
async fn the_refresh_endpoint_patches_only_the_charts(db: PgPool) {
	let (app, _) = app_and_state(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;
	let local = server_id(&db, team).await;
	sample(&db, local, 6, false).await;

	let (s, _, out) = get_html(
		&app,
		&format!("/teams/{team}/servers/{local}/monitoring/charts?range=24h"),
		Some(&cookie),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&out, "datastar-patch-elements");
	assert_contains(&out, "id=\"metrics-body\"");
	assert_contains(&out, "id=\"chart-cpu\"");
	assert_contains(&out, "CPU, last 24 hours.");
	// The frame (and its timer) is not re-sent, so the timer is not restarted.
	assert_not_contains(&out, "data-on-interval");
	assert_not_contains(&out, "<script");
}
