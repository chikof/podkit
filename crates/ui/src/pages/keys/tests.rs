//! The Keys page: what it shows, what it never shows, and who may act on it.

use axum::http::StatusCode;
use serde_json::json;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
};
use crate::tests::{PgPool, SSH_KEY, app, db_test, user_with_team};

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn keys_page_lists_keys_without_secrets_and_refuses_forged_posts(db: PgPool) {
	let app = app(db).await;
	let (alice, team) = ui_session(&app, "alice").await;
	let (s, _, _) = post_signals(
		&app,
		&format!("/teams/{team}/keys/import"),
		Some(&alice),
		&json!({ "imp_name": "laptop", "imp_key": SSH_KEY }),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _, page) = get_html(&app, &format!("/teams/{team}/keys"), Some(&alice)).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "<h1>Keys</h1>");
	assert_contains(&page, "laptop");
	assert_contains(&page, "SHA256:");
	assert_contains(&page, "ssh-ed25519 ");
	assert_eq!(page.matches("<h1").count(), 1);
	for secret in ["b3BlbnNzaC1rZXk", "BEGIN OPENSSH", "PRIVATE KEY"] {
		assert_not_contains(&page, secret);
	}

	// Someone outside the team gets nothing, and cannot make or delete keys there.
	let (_, _) = user_with_team(&app, "mallory").await;
	let mallory = ui_login(&app, "mallory").await;
	let (s, _, _) = get_html(&app, &format!("/teams/{team}/keys"), Some(&mallory)).await;
	assert!(
		s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
		"{s}"
	);
	let (s, _, body) = post_signals(
		&app,
		&format!("/teams/{team}/keys/generate"),
		Some(&mallory),
		&json!({ "gen_name": "evil", "gen_algorithm": "ed25519" }),
	)
	.await;
	// A refused action answers with its reason as a signal; no key was made.
	assert!(
		body.contains("_error") || s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
		"{s} {body}"
	);
	assert_not_contains(&body, "ssh-ed25519");
	let (_, _, page) = get_html(&app, &format!("/teams/{team}/keys"), Some(&alice)).await;
	assert_not_contains(&page, "evil");
}
