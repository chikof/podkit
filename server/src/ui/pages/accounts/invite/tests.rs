use axum::Router;
use axum::http::{StatusCode, header};
use serde_json::json;

use crate::state::Registration;
use crate::tests::{
	PgPool, app, app_with_registration, call, count, db_test, member_role, user_with_team,
};
use crate::ui::testing::{assert_contains, assert_not_contains, get_html, post_signals, ui_login};

const MEMBER: i64 = 2;

/// alice's team, and the path of an invite to `email` made through the API.
async fn invited(app: &Router, email: &str) -> (i64, String) {
	let (alice, team) = user_with_team(app, "alice").await;
	let (s, body) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/invites"),
		Some(&alice),
		Some(json!({ "email": email, "role_id": MEMBER })),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{body}");
	(team, body["link"].as_str().unwrap().to_owned())
}

fn signals() -> serde_json::Value {
	json!({ "name": "Dee", "password": "correct horse battery", "confirm": "correct horse battery" })
}

#[db_test]
async fn invite_page_signs_up_the_invited_address_on_a_closed_instance(db: PgPool) {
	let open = app(db.clone()).await;
	let (team, link) = invited(&open, "dee@example.com").await;
	let app = app_with_registration(db.clone(), Registration::Closed).await;

	let (s, h, page) = get_html(&app, &link, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(h[header::CACHE_CONTROL], "no-store");
	assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
	assert_eq!(h[header::CONTENT_SECURITY_POLICY], "frame-ancestors 'none'");
	assert_contains(&page, "Join alice");
	assert_contains(&page, "dee@example.com");
	assert_contains(&page, "Create account and join");

	let (s, h, body) = post_signals(&app, &link, None, &signals()).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(
		&body,
		&format!("window.location.assign('/teams/{team}/projects')"),
	);
	assert!(h.get(header::SET_COOKIE).is_some(), "signed in");
	assert_eq!(member_role(&db, team, "Dee").await, MEMBER);

	// Used up.
	let (s, _, page) = get_html(&app, &link, None).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_contains(&page, "This invite is not valid any more");
	let (_, _, body) = post_signals(&app, &link, None, &signals()).await;
	assert_contains(&body, "this invite is not valid any more");
	assert_eq!(count(&db, "users").await, 2);
}

#[db_test]
async fn invite_page_refuses_mismatched_passwords_and_odd_tokens(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, link) = invited(&app, "dee@example.com").await;
	let mut bad = signals();
	bad["confirm"] = json!("something else entirely");
	let (_, _, body) = post_signals(&app, &link, None, &bad).await;
	assert_contains(&body, "The passwords don't match.");
	assert_eq!(count(&db, "team_invites").await, 1);

	// Anything but podkit's hex is not looked up, and never written back into the page.
	for odd in ["/invite/x'%3Balert(1)%2F%2F", "/invite/ABC", "/invite/0"] {
		let (s, _, page) = get_html(&app, odd, None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{odd}");
		assert_not_contains(&page, "alert(1)");
	}
}

#[db_test]
async fn invite_page_lets_an_existing_account_accept_after_signing_in(db: PgPool) {
	let app = app(db.clone()).await;
	let creds =
		json!({ "name": "bob", "email": "bob@example.com", "password": "correct horse battery" });
	let (s, _) = call(&app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
	assert!(s.is_success());
	let (team, link) = invited(&app, "bob@example.com").await;

	let (s, headers, page) = get_html(&app, &link, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "Accept invite");
	// Sign-in comes back to plain `/invite`: the token waits in a cookie, not in the URL.
	assert_contains(&page, "\"/login?next=/invite\"");
	assert_not_contains(&page, &format!("/login?next={link}"));
	assert_not_contains(&page, "Create account and join");
	let cookie = headers
		.get_all(header::SET_COOKIE)
		.iter()
		.map(|v| v.to_str().unwrap())
		.find(|c| c.starts_with("invite="))
		.expect("the pending invite cookie");
	assert_contains(cookie, "HttpOnly");
	assert_contains(cookie, "SameSite=Strict");
	assert_contains(cookie, "Path=/invite");

	// Signed out: off to sign in, and back to `/invite` afterwards.
	let accept = format!("{link}/accept");
	let (_, _, body) = post_signals(&app, &accept, None, &json!({})).await;
	assert_contains(&body, "window.location.assign('/login?next=/invite')");
	assert_not_contains(&body, &format!("/login?next={link}"));
	assert_eq!(count(&db, "team_members").await, 1);

	// alice holds the link too, but it is not for her address.
	let alice = ui_login(&app, "alice").await;
	let (_, _, body) = post_signals(&app, &accept, Some(&alice), &json!({})).await;
	assert_contains(&body, "This invite is for bob@example.com");
	assert_eq!(count(&db, "team_invites").await, 1);

	let bob = ui_login(&app, "bob").await;
	let (_, _, body) = post_signals(&app, &accept, Some(&bob), &json!({})).await;
	assert_contains(
		&body,
		&format!("window.location.assign('/teams/{team}/projects')"),
	);
	assert_eq!(member_role(&db, team, "bob").await, MEMBER);
	assert_eq!(count(&db, "team_invites").await, 0);
}
