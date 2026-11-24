//! Private access through Tailscale: the sidecar's deploy request, the write-only auth key, the
//! jobs that start and remove it (against a fake agent), Headscale URLs, tenancy and the pages.
//! `cargo test -p podkit-core -p podkit-ui --all-features -- tailscale`
//! Joining a real tailnet needs an account, so that test is `#[ignore]`d: see `docs/tailscale.md`.
//!
//! The tests of `podkit_core::tailscale` that need the pages: they sign in through the
//! web UI or read what it renders, so they run against the whole router. The rest are beside
//! the code, in `podkit_core::tailscale::tests`.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
};
use crate::tests::{call, count, state, user_with_team};
use podkit_core::tests::tailscale::{KEY, Setup};

// --- the auth key is write-only ----------------------------------------------------------------

/// The shared [`Setup`] over the whole router: these tests read the pages.
async fn setup(db: PgPool) -> Setup {
	let state = state(db.clone());
	let app = crate::routes(state.clone()).await.unwrap();
	Setup::over(db, state, app).await
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn tailscale_auth_key_is_write_only_and_stored_encrypted(db: PgPool) {
	let s = setup(db).await;
	let (status, body) = s
		.put_settings(json!({
			"auth_key": KEY, "login_server": "https://hs.example.com/",
			"tags": "tag:podkit web",
		}))
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(
		body,
		json!({ "auth_key_set": true, "login_server": "https://hs.example.com",
			"tags": ["tag:podkit", "tag:web"] })
	);

	let (_, read) = call(&s.app, "GET", &s.settings_path(), Some(&s.token), None).await;
	assert_eq!(read["auth_key_set"], json!(true));
	assert!(!read.to_string().contains("tskey"));

	// At rest it is a blob that is not the key, and only the secret box opens it.
	let blob: Vec<u8> = sqlx::query_scalar("SELECT auth_key FROM tailscale_settings")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert!(!blob.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
	assert_eq!(s.state.secrets.decrypt(&blob).unwrap(), KEY);

	// Saving the other settings with an empty key keeps it.
	let (status, body) = s.put_settings(json!({ "tags": "tag:other" })).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["auth_key_set"], json!(true));
	assert_eq!(body["tags"], json!(["tag:other"]));
	assert_eq!(body["login_server"], Value::Null);

	// It reaches neither the audit log nor the page.
	let audit: Vec<String> = sqlx::query_scalar("SELECT diff::text FROM audit_events")
		.fetch_all(&s.db)
		.await
		.unwrap();
	assert!(!audit.is_empty());
	assert!(audit.iter().all(|d| !d.contains("tskey")), "{audit:?}");
	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events")
		.fetch_all(&s.db)
		.await
		.unwrap();
	assert!(actions.iter().any(|a| a == "tailscale.settings.update"));

	let cookie = ui_login(&s.app, "alice").await;
	let (status, _, page) = get_html(
		&s.app,
		&format!("/teams/{}/settings/tailscale", s.team),
		Some(&cookie),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_not_contains(&page, "tskey-auth-k");
	assert_contains(page_main(&page), "Stored");
	assert_eq!(page_main(&page).matches("<h1").count(), 1);

	// Clearing forgets it.
	let (status, body) = s
		.put_settings(json!({ "clear_auth_key": true, "tags": "tag:other" }))
		.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["auth_key_set"], json!(false));
}

// --- who may do what, and whose it is ---------------------------------------------------------

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn tailscale_other_teams_get_a_404_and_change_nothing(db: PgPool) {
	let s = setup(db).await;
	s.store_key().await;
	s.enable(json!({})).await;

	let (bob_token, bob_team) = user_with_team(&s.app, "bob").await;
	let (bob_project, _) = Setup::project_and_app(&s.app, &bob_token, bob_team, "mine").await;
	let id = s.application;

	// Bob uses alice's application id under his own team and project: a miss.
	let mine =
		format!("/api/v1/teams/{bob_team}/projects/{bob_project}/applications/{id}/tailscale");
	for (method, body) in [
		("GET", None),
		("PUT", Some(json!({ "hostname": "stolen" }))),
		("DELETE", None),
	] {
		let (status, answer) = call(&s.app, method, &mine, Some(&bob_token), body).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method}: {answer}");
	}
	// Under her team: refused, nothing leaked.
	for (method, path, body) in [
		("GET", s.exposure_path(), None),
		(
			"PUT",
			s.exposure_path(),
			Some(json!({ "hostname": "stolen" })),
		),
		("DELETE", s.exposure_path(), None),
		("GET", s.settings_path(), None),
		(
			"PUT",
			s.settings_path(),
			Some(json!({ "auth_key": "tskey-evil" })),
		),
	] {
		let (status, answer) = call(&s.app, method, &path, Some(&bob_token), body).await;
		assert!(
			matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
			"{method} {path}: {status} {answer}"
		);
		assert!(!answer.to_string().contains("shop"));
	}
	let (status, _) = call(&s.app, "GET", &s.settings_path(), None, None).await;
	assert!(status.is_client_error(), "{status}");

	// Bob's own team has its own settings: alice's key is not his.
	let (_, theirs) = call(
		&s.app,
		"GET",
		&format!("/api/v1/teams/{bob_team}/tailscale"),
		Some(&bob_token),
		None,
	)
	.await;
	assert_eq!(theirs["auth_key_set"], json!(false));

	assert_eq!(s.status().await["hostname"], json!("shop"));
	let key: Vec<u8> =
		sqlx::query_scalar("SELECT auth_key FROM tailscale_settings WHERE team_id = $1")
			.bind(s.team)
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert_eq!(s.state.secrets.decrypt(&key).unwrap(), KEY);

	// The pages: his cookie on her application's address is a 404 as well.
	let cookie = ui_login(&s.app, "bob").await;
	let (status, _, _) = get_html(
		&s.app,
		&format!("/teams/{bob_team}/projects/{bob_project}/apps/{id}/tailnet"),
		Some(&cookie),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn tailscale_pages_show_the_card_and_escape_what_they_show(db: PgPool) {
	let s = setup(db).await;
	let cookie = ui_login(&s.app, "alice").await;
	let team = s.team;
	let base = format!(
		"/teams/{team}/projects/{}/apps/{}",
		s.project, s.application
	);
	let domains = format!("{base}/domains");

	// Without a key the card points to Settings.
	let (status, _, page) = get_html(&s.app, &domains, Some(&cookie)).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&page);
	assert_contains(main, "Private access (Tailscale)");
	assert_contains(main, &format!("/teams/{team}/settings/tailscale"));
	assert_eq!(main.matches("<h1").count(), 1);

	// With one it offers a name, and turning it on from the UI queues the job.
	let (status, _, _) = post_signals(
		&s.app,
		&format!("/teams/{team}/settings/tailscale"),
		Some(&cookie),
		&json!({ "ts_key": KEY, "ts_login_server": "", "ts_tags": "tag:podkit" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	let (_, _, page) = get_html(&s.app, &domains, Some(&cookie)).await;
	assert_contains(page_main(&page), "Turn on");
	let (status, _, _) = post_signals(
		&s.app,
		&format!("{base}/tailnet"),
		Some(&cookie),
		&json!({ "tn_hostname": "shop" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(count(&s.db, "tailscale_exposures").await, 1);

	// An error message that came from elsewhere is escaped.
	sqlx::query("UPDATE tailscale_exposures SET status = 'failed', error = '<b>boom</b>'")
		.execute(&s.db)
		.await
		.unwrap();
	let (_, _, page) = get_html(&s.app, &domains, Some(&cookie)).await;
	assert_contains(page_main(&page), "&lt;b&gt;boom&lt;/b&gt;");
	assert_not_contains(page_main(&page), "<b>boom</b>");
	assert_not_contains(&page, "tskey-auth-k");
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn tailscale_a_plain_member_cannot_change_settings_or_exposure(db: PgPool) {
	let s = setup(db).await;
	let alice = ui_login(&s.app, "alice").await;
	let team = s.team;
	s.store_key().await;

	let creds =
		json!({ "name": "bob", "email": "bob@example.com", "password": "correct horse battery" });
	let (status, _) = call(&s.app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(status.is_success());
	let (status, _, _) = post_signals(
		&s.app,
		&format!("/teams/{team}/members"),
		Some(&alice),
		&json!({ "email": "bob@example.com", "newrole": "2" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	let bob = ui_login(&s.app, "bob").await;

	// The page shows state but no form; a forged POST changes nothing.
	let settings = format!("/teams/{team}/settings/tailscale");
	let (status, _, page) = get_html(&s.app, &settings, Some(&bob)).await;
	assert_eq!(status, StatusCode::OK);
	assert_not_contains(page_main(&page), "ts_key");
	assert_not_contains(&page, "tskey-auth-k");
	post_signals(
		&s.app,
		&settings,
		Some(&bob),
		&json!({ "ts_key": "tskey-forged", "ts_login_server": "https://evil.example", "ts_tags": "" }),
	)
	.await;
	let blob: Vec<u8> = sqlx::query_scalar("SELECT auth_key FROM tailscale_settings")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(s.state.secrets.decrypt(&blob).unwrap(), KEY);
	let server: Option<String> = sqlx::query_scalar("SELECT login_server FROM tailscale_settings")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(server, None);

	let enable = format!(
		"/teams/{team}/projects/{}/apps/{}/tailnet",
		s.project, s.application
	);
	post_signals(
		&s.app,
		&enable,
		Some(&bob),
		&json!({ "tn_hostname": "shop" }),
	)
	.await;
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
}
