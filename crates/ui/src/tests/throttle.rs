//! Sign-in throttling (SEC-01): the limiter itself with a clock we move, then every place a
//! guess can be made. `cargo test -p podkit-core -p podkit-ui --all-features -- throttle`
//!
//! The tests of `podkit_core::throttle` that need the pages: they sign in through the
//! web UI or read what it renders, so they run against the whole router. The rest are beside
//! the code, in `podkit_core::throttle::tests`.

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

use crate::tests::{PgPool, db_test, state, user_id, user_with_team};
use podkit_core::state::AppState;
use podkit_core::tests::throttle::{PASSWORD, hold, login, with_totp};
use podkit_core::throttle::ACCOUNT_LIMIT;

async fn world(db: PgPool) -> (AppState, Router) {
	let st = state(db);
	let app = crate::routes(st.clone()).await.unwrap();
	(st, app)
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn throttle_the_members_page_shows_the_lockout_and_the_owner_can_unlock_it(db: PgPool) {
	use crate::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};

	let (_st, app) = world(db.clone()).await;
	let (alice, team) = ui_session(&app, "alice").await;
	user_with_team(&app, "bob").await;
	let bob = user_id(&db, "bob").await;
	let (status, _, _) = post_signals(
		&app,
		&format!("/teams/{team}/members"),
		Some(&alice),
		&json!({ "email": "bob@example.com", "newrole": "2" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	for _ in 0..ACCOUNT_LIMIT {
		login(&app, "bob@example.com", "wrong password", None).await;
	}

	hold(&db).await;
	let page = format!("/teams/{team}/members");
	let (_, _, html) = get_html(&app, &page, Some(&alice)).await;
	assert_contains(&html, "Locked, ");
	assert_contains(&html, "Unlock");
	assert_eq!(html.matches("<h1").count(), 1, "one h1 per page");

	let (status, _, body) = post_signals(
		&app,
		&format!("{page}/unlock"),
		Some(&alice),
		&json!({ "target": bob.to_string() }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "Sign-in unlocked");
	let (_, _, html) = get_html(&app, &page, Some(&alice)).await;
	assert_not_contains(&html, "Locked, ");
	let cookie = ui_login(&app, "bob").await;
	assert!(cookie.starts_with("session="));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn throttle_the_web_sign_in_says_so_in_words(db: PgPool) {
	use crate::testing::{Browser, assert_contains};

	let (_st, app) = world(db.clone()).await;
	user_with_team(&app, "bob").await;
	let mut b = Browser::default();
	for _ in 0..ACCOUNT_LIMIT {
		b.post(
			&app,
			"/login",
			&json!({ "email": "bob@example.com", "password": "nope" }),
		)
		.await;
	}
	hold(&db).await;
	let (status, _, body) = b
		.post(
			&app,
			"/login",
			&json!({ "email": "bob@example.com", "password": PASSWORD }),
		)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "Too many attempts, try again in ");
	assert!(!b.has_session());
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn throttle_the_web_second_step_counts_codes_across_new_sign_ins(db: PgPool) {
	use crate::testing::Browser;

	let (st, app) = world(db.clone()).await;
	let (_ann, _secret, _) = with_totp(&st, &app, "ann").await;
	let creds = json!({ "email": "ann@example.com", "password": PASSWORD });
	// Starting the second step over and over (the flow's own five tries reset each time)
	// does not buy more guesses: the account has its own count.
	for _ in 0..ACCOUNT_LIMIT {
		let mut b = Browser::default();
		b.post(&app, "/login", &creds).await;
		b.post(
			&app,
			"/login/two-step",
			&json!({ "code": "000000", "recovery": "" }),
		)
		.await;
		assert!(!b.has_session());
	}
	hold(&db).await;
	let mut b = Browser::default();
	let (_, _, body) = b.post(&app, "/login", &creds).await;
	assert!(
		body.contains("Too many attempts"),
		"even the password step: {body}"
	);
	assert!(!b.jar.contains_key("pk_2fa"));
}
