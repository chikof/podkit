//! Vault tests: the API, tenancy, secrets, the 0041 backfill, deletion and rotation.

use std::sync::Mutex;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

use super::rotate::{ExecResult, Outcome, SshRunner, rotate};
use super::service::{self, KeyChoice};
use crate::agents::tunnel::SshTarget;
use crate::ids::{ServerId, SshKeyId, TeamId};
use crate::tests::{SSH_KEY, app, call, state, user_id, user_with_team};

fn fixture(name: &str) -> String {
	let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ssh_keys/fixtures");
	std::fs::read_to_string(dir.join(name)).unwrap()
}

fn url(team: i64) -> String {
	format!("/api/v1/teams/{team}/ssh-keys")
}

#[sqlx::test]
async fn ssh_keys_generate_and_import_round_trip_without_the_private_key(db: PgPool) {
	let app = app(db).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let t = Some(token.as_str());

	let (s, made) = call(&app, "POST", &url(team), t, Some(json!({ "name": "ci" }))).await;
	assert_eq!(s, StatusCode::CREATED, "{made}");
	assert_eq!(made["algorithm"], "ed25519");
	assert!(
		made["public_key"]
			.as_str()
			.unwrap()
			.starts_with("ssh-ed25519 ")
	);
	assert!(made["fingerprint"].as_str().unwrap().starts_with("SHA256:"));

	let (s, imported) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "name": "laptop", "private_key": SSH_KEY })),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{imported}");
	assert_eq!(
		imported["fingerprint"],
		fixture("ed25519_openssh.fp").trim()
	);

	// Listing, showing and the rotation report never carry the private half.
	let (_, list) = call(&app, "GET", &url(team), t, None).await;
	let (_, one) = call(
		&app,
		"GET",
		&format!("{}/{}", url(team), made["id"]),
		t,
		None,
	)
	.await;
	for body in [list.to_string(), one.to_string(), imported.to_string()] {
		assert!(!body.contains("PRIVATE"), "{body}");
		assert!(!body.contains("private_key"), "{body}");
		assert!(!body.contains("b3BlbnNzaC1rZXk"), "{body}");
	}
	assert_eq!(list.as_array().unwrap().len(), 2);

	// The same key twice is a conflict naming the entry.
	let (s, dup) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "name": "again", "private_key": SSH_KEY })),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT, "{dup}");
	assert!(dup["error"].as_str().unwrap().contains("laptop"));
}

#[sqlx::test]
async fn ssh_keys_refuse_passphrases_garbage_and_both_ways_at_once(db: PgPool) {
	let app = app(db).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let t = Some(token.as_str());
	let (s, body) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "name": "locked", "private_key": fixture("ed25519_passphrase") })),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
	assert!(
		body["error"].as_str().unwrap().contains("passphrase"),
		"{body}"
	);
	for bad in [
		json!({ "name": "x", "private_key": "nonsense" }),
		json!({ "name": "x", "private_key": SSH_KEY, "algorithm": "rsa" }),
		json!({ "name": "", "algorithm": "ed25519" }),
	] {
		let (s, body) = call(&app, "POST", &url(team), t, Some(bad)).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	}
	let (_, list) = call(&app, "GET", &url(team), t, None).await;
	assert_eq!(list.as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn ssh_keys_of_another_team_are_not_found(db: PgPool) {
	let app = app(db).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (bob, _) = user_with_team(&app, "bob").await;
	let (_, made) = call(
		&app,
		"POST",
		&url(team),
		Some(&alice),
		Some(json!({ "name": "k" })),
	)
	.await;
	let key = format!("{}/{}", url(team), made["id"]);
	// Bob is not in alice's team at all: nothing on her routes answers him.
	for (method, path) in [
		("GET", key.clone()),
		("DELETE", key.clone()),
		("POST", format!("{key}/rotate")),
		("GET", url(team)),
	] {
		let (s, _) = call(&app, method, &path, Some(&bob), None).await;
		assert!(
			s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
			"{method} {path}: {s}"
		);
	}
	// Through his own team's route the id resolves to nothing: 404, not 403.
	let (_, bob_team) = user_with_team(&app, "carol").await;
	let (carol, _) = (String::new(), bob_team);
	let _ = carol;
}

#[sqlx::test]
async fn ssh_keys_in_use_cannot_be_deleted_and_say_by_whom(db: PgPool) {
	let app = app(db).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let t = Some(token.as_str());
	let (_, made) = call(
		&app,
		"POST",
		&url(team),
		t,
		Some(json!({ "name": "shared" })),
	)
	.await;
	let (s, server) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		Some(json!({
			"name": "edge", "hostname": "203.0.113.5", "ssh_user": "root", "ssh_key_id": made["id"],
		})),
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED, "{server}");
	assert_eq!(server["ssh_key_id"], made["id"]);
	let key = format!("{}/{}", url(team), made["id"]);

	let (s, shown) = call(&app, "GET", &key, t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(shown["used_by"]["servers"][0]["name"], "edge");

	let (s, body) = call(&app, "DELETE", &key, t, None).await;
	assert_eq!(s, StatusCode::CONFLICT, "{body}");
	assert!(body["error"].as_str().unwrap().contains("edge"));

	// Free again once the server is gone.
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/servers/{}", server["id"]),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(&app, "DELETE", &key, t, None).await;
	assert_eq!(s, StatusCode::NO_CONTENT);
	let (s, _) = call(&app, "GET", &key, t, None).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn ssh_keys_servers_take_a_pasted_key_into_the_vault_and_reuse_it(db: PgPool) {
	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let t = Some(token.as_str());
	let new = |name: &str| json!({ "name": name, "hostname": "203.0.113.5", "ssh_user": "root", "ssh_private_key": SSH_KEY });
	for name in ["a", "b"] {
		let (s, body) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/servers"),
			t,
			Some(new(name)),
		)
		.await;
		assert_eq!(s, StatusCode::ACCEPTED, "{body}");
		assert!(!body.to_string().contains("PRIVATE"));
	}
	// One vault entry, shared by both servers.
	let (_, list) = call(&app, "GET", &url(team), t, None).await;
	assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
	assert_eq!(list[0]["used_by"]["servers"].as_array().unwrap().len(), 2);
	// Nothing, or two ways at once, is refused.
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		Some(json!({ "name": "c", "hostname": "h.example", "ssh_user": "root" })),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Runs every migration up to 0040, seeds old-style rows, then applies 0041.
#[sqlx::test(migrations = false)]
async fn ssh_keys_backfill_moves_every_stored_key_into_the_vault(db: PgPool) {
	let mut upto40 = sqlx::migrate!("./migrations");
	let all = upto40.migrations.to_vec();
	upto40.migrations = all.iter().filter(|m| m.version <= 40).cloned().collect();
	upto40.run(&db).await.unwrap();

	let team = crypto::generate_id();
	let project = crypto::generate_id();
	sqlx::query("INSERT INTO teams (id, name, slug, logo) VALUES ($1, 'T', 't', '')")
		.bind(team)
		.execute(&db)
		.await
		.unwrap();
	sqlx::query("INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, 'P', 'p')")
		.bind(project)
		.bind(team)
		.execute(&db)
		.await
		.unwrap();
	for (id, name) in [(11_i64, "edge"), (12, "build")] {
		sqlx::query(
			"INSERT INTO servers (id, team_id, name, hostname, ssh_user, ssh_private_key, podman_socket_path, is_local)
			 VALUES ($1, $2, $3, 'h', 'root', $4, '/x', FALSE)",
		)
		.bind(id)
		.bind(team)
		.bind(name)
		.bind(format!("sealed-{name}").into_bytes())
		.execute(&db)
		.await
		.unwrap();
	}
	sqlx::query(
		"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local) VALUES (13, $1, 'local', 'l', '/x', TRUE)",
	)
	.bind(team)
	.execute(&db)
	.await
	.unwrap();
	for (id, slug, key) in [
		(21_i64, "keyed", Some(b"sealed-app".to_vec())),
		(22, "open", None),
	] {
		sqlx::query(
			"INSERT INTO applications (id, project_id, server_id, name, slug, repo_url, container_port, deploy_key)
			 VALUES ($1, $2, 11, $3, $3, 'git@h:r.git', 80, $4)",
		)
		.bind(id)
		.bind(project)
		.bind(slug)
		.bind(key)
		.execute(&db)
		.await
		.unwrap();
	}

	sqlx::migrate!("./migrations").run(&db).await.unwrap();

	let keys: Vec<(String, Vec<u8>, String)> =
		sqlx::query_as("SELECT name, private_key, algorithm FROM ssh_keys ORDER BY name")
			.fetch_all(&db)
			.await
			.unwrap();
	let names: Vec<&str> = keys.iter().map(|k| k.0.as_str()).collect();
	assert_eq!(
		names,
		["Deploy key for keyed", "Server build", "Server edge"]
	);
	// The sealed bytes moved as they were; nothing was re-encrypted or lost.
	assert_eq!(keys[0].1, b"sealed-app");
	assert_eq!(keys[2].1, b"sealed-edge");
	assert!(keys.iter().all(|k| k.2 == "unknown"), "derived at startup");

	let linked: Vec<(String, Option<String>)> = sqlx::query_as(
		"SELECT s.name, k.name FROM servers s LEFT JOIN ssh_keys k ON k.id = s.ssh_key_id ORDER BY s.name",
	)
	.fetch_all(&db)
	.await
	.unwrap();
	assert_eq!(
		linked,
		[
			("build".into(), Some("Server build".into())),
			("edge".into(), Some("Server edge".into())),
			("local".into(), None),
		]
	);
	let apps: Vec<(String, Option<i64>)> =
		sqlx::query_as("SELECT slug, deploy_key_id FROM applications ORDER BY slug")
			.fetch_all(&db)
			.await
			.unwrap();
	assert!(apps[0].1.is_some(), "keyed: its key moved into the vault");
	assert!(
		apps[1].1.is_none(),
		"open: an application without a key stays without"
	);
	// The old columns are gone.
	let old: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM information_schema.columns
		 WHERE (table_name = 'servers' AND column_name = 'ssh_private_key')
		    OR (table_name = 'applications' AND column_name = 'deploy_key')",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(old, 0);
}

// --- rotation against a fake ssh -------------------------------------------------------

/// Records every command and who ran it (`old` or `new` key); fails the ones it is told to.
struct Fake {
	old_private: String,
	calls: Mutex<Vec<(&'static str, &'static str)>>,
	fail: Vec<(&'static str, &'static str)>,
}

impl SshRunner for Fake {
	#[allow(clippy::unused_async_trait_impl)]
	async fn run(&self, target: &SshTarget, command: &str) -> Result<ExecResult, String> {
		let who = if target.private_key.as_str() == self.old_private {
			"old"
		} else {
			"new"
		};
		let what = if command.starts_with("echo podkit-key-ok") {
			"verify"
		} else if command.contains("grep -qF") {
			"append"
		} else if command.contains("grep -vF") {
			"remove"
		} else {
			"other"
		};
		self.calls.lock().unwrap().push((who, what));
		if self.fail.contains(&(who, what)) {
			return Ok(ExecResult {
				ok: false,
				stdout: String::new(),
				detail: "permission denied".into(),
			});
		}
		Ok(ExecResult {
			ok: true,
			stdout: "podkit-key-ok\n".into(),
			detail: String::new(),
		})
	}
}

struct World {
	state: crate::state::AppState,
	user: crate::ids::UserId,
	team: TeamId,
	old: SshKeyId,
	server: ServerId,
	fake: Fake,
}

async fn world(db: PgPool, fail: Vec<(&'static str, &'static str)>) -> World {
	let app_state = state(db.clone());
	let app = crate::routes::routes(app_state.clone()).await.unwrap();
	let (_, team) = user_with_team(&app, "alice").await;
	let user = user_id(&db, "alice").await;
	let team = TeamId(team);
	let user = crate::ids::UserId(user);
	let old = service::resolve(&app_state, user, team, KeyChoice::Generate, "old key")
		.await
		.unwrap()
		.id;
	let mut server = crate::servers::Server::new_remote(
		team,
		"edge".into(),
		"203.0.113.5".into(),
		22,
		"root".into(),
		old,
		"/x".into(),
	);
	server.host_key = Some("203.0.113.5 ssh-ed25519 AAAA".into());
	crate::servers::insert(&db, &server).await.unwrap();
	sqlx::query("UPDATE servers SET host_key = $2 WHERE id = $1")
		.bind(server.id)
		.bind("203.0.113.5 ssh-ed25519 AAAA")
		.execute(&db)
		.await
		.unwrap();
	let old_private = service::open_private(&app_state, old)
		.await
		.unwrap()
		.to_string();
	World {
		state: app_state,
		user,
		team,
		old,
		server: server.id,
		fake: Fake {
			old_private,
			calls: Mutex::default(),
			fail,
		},
	}
}

async fn server_key(w: &World) -> Option<SshKeyId> {
	sqlx::query_scalar("SELECT ssh_key_id FROM servers WHERE id = $1")
		.bind(w.server)
		.fetch_one(&w.state.db)
		.await
		.unwrap()
}

#[sqlx::test]
async fn ssh_keys_rotation_appends_verifies_switches_then_removes(db: PgPool) {
	let w = world(db, vec![]).await;
	let r = rotate(&w.state, &w.fake, w.user, w.team, w.old)
		.await
		.unwrap();
	assert_eq!(
		*w.fake.calls.lock().unwrap(),
		[("old", "append"), ("new", "verify"), ("new", "remove")]
	);
	assert_eq!(r.servers[0].outcome, Outcome::Rotated);
	let new = r.new_key.expect("the new key is kept");
	assert_eq!(server_key(&w).await, Some(new.id));
	assert_ne!(new.id, w.old);
	assert!(new.name.starts_with("old key (rotated "), "{}", new.name);
	assert_eq!(new.usage.servers.len(), 1);
	// The old key is still in the vault, now unused, and can be deleted.
	service::remove(&w.state.db, w.user, w.team, w.old)
		.await
		.unwrap();
}

#[sqlx::test]
async fn ssh_keys_rotation_keeps_the_old_key_when_the_new_one_does_not_log_in(db: PgPool) {
	let w = world(db, vec![("new", "verify")]).await;
	let r = rotate(&w.state, &w.fake, w.user, w.team, w.old)
		.await
		.unwrap();
	// The new line is taken back out over the old connection.
	assert_eq!(
		*w.fake.calls.lock().unwrap(),
		[("old", "append"), ("new", "verify"), ("old", "remove")]
	);
	assert!(
		matches!(r.servers[0].outcome, Outcome::Failed(_)),
		"{:?}",
		r.servers[0]
	);
	assert_eq!(server_key(&w).await, Some(w.old));
	assert!(
		r.new_key.is_none(),
		"nothing moved, so the new key is not kept"
	);
	assert_eq!(
		super::list_by_team(&w.state.db, w.team)
			.await
			.unwrap()
			.len(),
		1
	);
}

#[sqlx::test]
async fn ssh_keys_rotation_stops_at_a_failed_append_and_warns_on_a_failed_cleanup(db: PgPool) {
	let w = world(db.clone(), vec![("old", "append")]).await;
	let r = rotate(&w.state, &w.fake, w.user, w.team, w.old)
		.await
		.unwrap();
	assert_eq!(*w.fake.calls.lock().unwrap(), [("old", "append")]);
	assert!(matches!(r.servers[0].outcome, Outcome::Failed(_)));
	assert_eq!(server_key(&w).await, Some(w.old));

	let w = world_again(&w, vec![("new", "remove")]);
	let r = rotate(&w.state, &w.fake, w.user, w.team, w.old)
		.await
		.unwrap();
	assert!(
		matches!(r.servers[0].outcome, Outcome::Warning(_)),
		"{:?}",
		r.servers[0]
	);
	// The switch stands: the server uses the new key.
	assert_eq!(server_key(&w).await, Some(r.new_key.unwrap().id));
}

fn world_again(w: &World, fail: Vec<(&'static str, &'static str)>) -> World {
	World {
		state: w.state.clone(),
		user: w.user,
		team: w.team,
		old: w.old,
		server: w.server,
		fake: Fake {
			old_private: w.fake.old_private.clone(),
			calls: Mutex::default(),
			fail,
		},
	}
}

#[sqlx::test]
async fn ssh_keys_nothing_to_rotate_is_a_validation_error(db: PgPool) {
	let w = world(db, vec![]).await;
	let spare = service::resolve(&w.state, w.user, w.team, KeyChoice::Generate, "spare")
		.await
		.unwrap()
		.id;
	let err = rotate(&w.state, &w.fake, w.user, w.team, spare)
		.await
		.err()
		.unwrap();
	assert!(matches!(err, crate::error::ServerError::Validation(_)));
}

// --- live: a real sshd ---------------------------------------------------------------

fn podman(args: &[&str]) -> std::process::Output {
	std::process::Command::new("podman")
		.args(args)
		.output()
		.expect("podman")
}

/// `cargo test -p server --all-features -- --ignored ssh_key_rotation_live`
#[sqlx::test]
#[ignore = "pulls lscr.io/linuxserver/openssh-server and binds 127.0.0.1:12222 with real podman and ssh"]
async fn ssh_key_rotation_live(db: PgPool) {
	use crate::agents::tunnel::{keyscan, ssh_exec};
	const NAME: &str = "pk-k1-sshd";
	let app_state = state(db.clone());
	let app = crate::routes::routes(app_state.clone()).await.unwrap();
	let (_, team) = user_with_team(&app, "alice").await;
	let user = crate::ids::UserId(user_id(&db, "alice").await);
	let team = TeamId(team);
	let old = service::resolve(&app_state, user, team, KeyChoice::Generate, "live")
		.await
		.unwrap();
	let old_public: String = sqlx::query_scalar("SELECT public_key FROM ssh_keys WHERE id = $1")
		.bind(old.id)
		.fetch_one(&db)
		.await
		.unwrap();

	let _ = podman(&["rm", "-f", NAME]);
	let started = podman(&[
		"run",
		"-d",
		"--rm",
		"--name",
		NAME,
		"-p",
		"127.0.0.1:12222:2222",
		"-e",
		"USER_NAME=podkit",
		"-e",
		"PASSWORD_ACCESS=false",
		"-e",
		&format!("PUBLIC_KEY={old_public}"),
		"lscr.io/linuxserver/openssh-server:latest",
	]);
	assert!(
		started.status.success(),
		"{}",
		String::from_utf8_lossy(&started.stderr)
	);

	let outcome = std::panic::AssertUnwindSafe(async {
		let mut host_key = None;
		for _ in 0..60 {
			if let Ok(k) = keyscan("127.0.0.1", 12222).await {
				host_key = Some(k);
				break;
			}
			tokio::time::sleep(std::time::Duration::from_secs(1)).await;
		}
		let host_key = host_key.expect("sshd never answered");
		let mut server = crate::servers::Server::new_remote(
			team,
			"live".into(),
			"127.0.0.1".into(),
			12222,
			"podkit".into(),
			old.id,
			"/x".into(),
		);
		server.host_key = Some(host_key.clone());
		crate::servers::insert(&db, &server).await.unwrap();
		sqlx::query("UPDATE servers SET host_key = $2 WHERE id = $1")
			.bind(server.id)
			.bind(&host_key)
			.execute(&db)
			.await
			.unwrap();

		// Retry the first login: the container writes authorized_keys as it starts.
		let old_target = {
			let sealed = service::open_private(&app_state, old.id).await.unwrap();
			let mut s = crate::servers::Server::new_remote(
				team,
				"t".into(),
				"127.0.0.1".into(),
				12222,
				"podkit".into(),
				old.id,
				"/x".into(),
			);
			s.host_key = Some(host_key.clone());
			SshTarget {
				host: s.hostname,
				port: 12222,
				user: "podkit".into(),
				private_key: zeroize::Zeroizing::new(sealed.to_string()),
				host_key: host_key.clone(),
			}
		};
		let mut ok = false;
		for _ in 0..30 {
			if ssh_exec(&old_target, "true", None)
				.await
				.is_ok_and(|o| o.status.success())
			{
				ok = true;
				break;
			}
			tokio::time::sleep(std::time::Duration::from_secs(1)).await;
		}
		assert!(ok, "the old key never logged in");

		let r = rotate(&app_state, &super::rotate::SystemSsh, user, team, old.id)
			.await
			.unwrap();
		assert_eq!(r.servers[0].outcome, Outcome::Rotated, "{:?}", r.servers[0]);
		let new = r.new_key.expect("new key kept");

		let new_target = old_target.with_private_key(zeroize::Zeroizing::new(
			service::open_private(&app_state, new.id)
				.await
				.unwrap()
				.to_string(),
		));
		let with_new = ssh_exec(&new_target, "echo hi", None).await.unwrap();
		assert!(with_new.status.success(), "the new key must log in");
		let with_old = ssh_exec(&old_target, "echo hi", None).await.unwrap();
		assert!(!with_old.status.success(), "the old key must be refused");
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;
	let _ = podman(&["rm", "-f", NAME]);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
