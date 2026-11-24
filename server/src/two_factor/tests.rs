//! Two-step verification end to end: an authenticator app and recovery codes at sign-in (JSON
//! API and web), passkeys through a soft authenticator, the team policy and its grace
//! period, and the session list. Every test name holds `two_factor`, so `-- two_factor`
//! runs them (and the unit tests next to `totp` and `service`).

#![allow(
	clippy::many_single_char_names,
	clippy::too_many_lines,
	reason = "a table of scenarios reads best straight down"
)]

use axum::Router;
use axum::http::{StatusCode, header};
use serde_json::{Value, json};
use time::OffsetDateTime;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::Url;

use super::service::{self, RECOVERY_CODES};
use super::totp;
use super::{passkeys, policy};
use crate::auth::flow::MAX_ATTEMPTS;
use crate::error::ServerError;
use crate::ids::{RoleId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{PgPool, call, count, db_test, state, user_id, user_with_team};
use crate::ui::testing::{Browser, get_html, page_main, post_signals, send, ui_session};

const PASSWORD: &str = "correct horse battery";
const ORIGIN: &str = "https://podkit.test";

async fn world(db: PgPool) -> (AppState, Router) {
	let mut st = state(db);
	let mut config = (*st.config).clone();
	config.public_url = Some(ORIGIN.to_owned());
	st.config = std::sync::Arc::new(config);
	let app = crate::routes::routes(st.clone()).await.unwrap();
	(st, app)
}

fn creds(name: &str) -> Value {
	json!({ "email": format!("{name}@example.com"), "password": PASSWORD, "next": "/" })
}

fn unix() -> u64 {
	u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap()
}

/// The code the app shows `offset` 30-second steps from now.
fn code(secret_b32: &str, offset: i64) -> String {
	let secret = totp::base32_decode(secret_b32).unwrap();
	let step = totp::step_of(unix()).checked_add_signed(offset).unwrap();
	format!("{:06}", totp::code_at(&secret, step, totp::DIGITS))
}

/// Turns the authenticator app on for `user`. Returns the secret (base32) and the recovery
/// codes. The confirming code is the previous step's, so the current one and the next are
/// still unspent.
async fn enrol(st: &AppState, user: UserId) -> (String, Vec<String>) {
	let setup = service::begin_totp(st, user).await.unwrap();
	let codes = service::confirm_totp(st, user, &code(&setup.secret, -1))
		.await
		.unwrap();
	(setup.secret, codes)
}

async fn uid(db: &PgPool, name: &str) -> UserId {
	UserId(user_id(db, name).await)
}

async fn audit_actions(db: &PgPool) -> Vec<String> {
	sqlx::query_scalar("SELECT action FROM audit_events ORDER BY id")
		.fetch_all(db)
		.await
		.unwrap()
}

// --- the JSON API ----------------------------------------------------------------------

#[db_test]
async fn two_factor_api_login_needs_a_code_once_the_app_is_on(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	user_with_team(&app, "ann").await;
	user_with_team(&app, "bob").await;
	let ann = uid(&db, "ann").await;
	let (secret, recovery) = enrol(&st, ann).await;
	assert_eq!(recovery.len(), RECOVERY_CODES);

	let login = |body: Value| {
		let app = app.clone();
		async move { call(&app, "POST", "/api/v1/auth/login", None, Some(body)).await }
	};
	let with = |c: &str| json!({ "email": "ann@example.com", "password": PASSWORD, "code": c });

	// No code: told to send one, no token.
	let (status, body) = login(json!({ "email": "ann@example.com", "password": PASSWORD })).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);
	assert!(body.get("token").is_none(), "{body}");
	// A wrong one.
	let (status, body) = login(with("000001")).await;
	assert!(
		status == StatusCode::UNAUTHORIZED && body.get("token").is_none(),
		"{status} {body}"
	);
	// A right one, once.
	let now = code(&secret, 0);
	let (status, body) = login(with(&now)).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(body["token"].is_string());
	let (status, _) = login(with(&now)).await;
	assert_eq!(
		status,
		StatusCode::UNAUTHORIZED,
		"a code is spent by its first use"
	);
	// The next step's code is fine: the clock may be a little off.
	let (status, _) = login(with(&code(&secret, 1))).await;
	assert_eq!(status, StatusCode::OK);
	// Far from now is not.
	let (status, _) = login(with(&code(&secret, 5))).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);

	// A recovery code stands in for the app, once.
	let (status, _) = login(with(&recovery[0])).await;
	assert_eq!(status, StatusCode::OK);
	let (status, _) = login(with(&recovery[0])).await;
	assert_eq!(
		status,
		StatusCode::UNAUTHORIZED,
		"a recovery code works once"
	);
	assert_eq!(service::status(&db, ann).await.unwrap().recovery_left, 9);

	// Someone without the app signs in as before.
	let (status, _) = call(
		&app,
		"POST",
		"/api/v1/auth/login",
		None,
		Some(json!({ "email": "bob@example.com", "password": PASSWORD })),
	)
	.await;
	assert_eq!(status, StatusCode::OK);

	let actions = audit_actions(&db).await;
	assert!(
		actions.iter().any(|a| a == "session.login_failed"),
		"{actions:?}"
	);
	assert!(
		actions.iter().any(|a| a == "two_factor.enable"),
		"{actions:?}"
	);
}

// --- the web sign-in -------------------------------------------------------------------

#[db_test]
async fn two_factor_web_sign_in_waits_for_the_second_step(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	user_with_team(&app, "ann").await;
	let ann = uid(&db, "ann").await;
	let (secret, recovery) = enrol(&st, ann).await;

	// The password alone opens nothing.
	let mut b = Browser::default();
	let (status, _, body) = b.post(&app, "/login", &creds("ann")).await;
	assert_eq!(status, StatusCode::OK);
	assert!(body.contains("/login/two-step"), "{body}");
	assert!(!b.has_session());
	assert!(b.jar.contains_key("pk_2fa"));
	let (status, _, page) = b.get(&app, "/login/two-step").await;
	assert_eq!(status, StatusCode::OK);
	assert!(page.contains("Two-step verification"));
	assert_eq!(page.matches("<h1").count(), 1, "one h1 per page");

	// Five wrong codes end the attempt; then even the right one is too late.
	for _ in 0..MAX_ATTEMPTS {
		b.post(
			&app,
			"/login/two-step",
			&json!({ "code": "123456", "recovery": "" }),
		)
		.await;
	}
	b.post(
		&app,
		"/login/two-step",
		&json!({ "code": code(&secret, 0), "recovery": "" }),
	)
	.await;
	assert!(!b.has_session());

	// A fresh attempt with the right code signs in; the half-done cookie is gone.
	let mut ok = Browser::default();
	ok.post(&app, "/login", &creds("ann")).await;
	let (status, _, body) = ok
		.post(
			&app,
			"/login/two-step",
			&json!({ "code": code(&secret, 0), "recovery": "" }),
		)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert!(body.contains("window.location"), "{body}");
	assert!(ok.has_session());
	assert!(!ok.jar.contains_key("pk_2fa"));

	// A recovery code, once.
	let mut r = Browser::default();
	r.post(&app, "/login", &creds("ann")).await;
	r.post(
		&app,
		"/login/two-step",
		&json!({ "code": "", "recovery": recovery[3] }),
	)
	.await;
	assert!(r.has_session());
	let mut again = Browser::default();
	again.post(&app, "/login", &creds("ann")).await;
	again
		.post(
			&app,
			"/login/two-step",
			&json!({ "code": "", "recovery": recovery[3] }),
		)
		.await;
	assert!(!again.has_session(), "a recovery code works once");

	// Nobody can reach the second step without the password first.
	let mut none = Browser::default();
	let (status, headers, _) = none.get(&app, "/login/two-step").await;
	assert!(status.is_redirection());
	assert_eq!(headers[header::LOCATION], "/login");
}

// --- team policy -----------------------------------------------------------------------

#[db_test]
async fn two_factor_team_policy_shuts_out_members_after_the_grace_period(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	let (_, team) = user_with_team(&app, "own").await;
	let (mem_token, mem_team) = user_with_team(&app, "mem").await;
	let (out_cookie, out_team) = ui_session(&app, "out").await;
	let own = uid(&db, "own").await;
	let mem = uid(&db, "mem").await;
	let out = uid(&db, "out").await;
	let team_id = TeamId(team);
	crate::members::insert(&db, team_id, mem, RoleId(2))
		.await
		.unwrap();
	let own_cookie = crate::ui::testing::ui_login(&app, "own").await;
	let mem_cookie = crate::ui::testing::ui_login(&app, "mem").await;
	let page = format!("/teams/{team}/settings/authentication");
	let account = format!("/teams/{team}/account/security");

	// The owner must have their own second step before shutting people out at once.
	let refused = policy::set(&db, own, team_id, true, 0).await;
	assert!(
		matches!(refused, Err(ServerError::Validation(_))),
		"{refused:?}"
	);
	enrol(&st, own).await;

	// With a grace period members carry on, and are told how long they have.
	let set = policy::set(&db, own, team_id, true, 7).await.unwrap();
	assert!(set.required && set.deadline.is_some());
	assert!(matches!(
		policy::standing(&db, mem, team_id).await.unwrap(),
		policy::Standing::Grace(_)
	));
	let (status, _, _) = get_html(&app, &page, Some(&mem_cookie)).await;
	assert_eq!(status, StatusCode::OK);
	let (status, _, security) = get_html(&app, &account, Some(&mem_cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert!(
		security.contains("requires two-step verification"),
		"the banner"
	);

	// Past the deadline: the team's pages send the member to Account, the API says 403,
	// and the Account pages still open so they can fix it.
	sqlx::query(
		"UPDATE teams SET require_2fa_deadline = NOW() - INTERVAL '1 minute' WHERE id = $1",
	)
	.bind(team)
	.execute(&db)
	.await
	.unwrap();
	assert!(policy::blocks(&db, mem, team_id).await.unwrap());
	assert!(
		!policy::blocks(&db, own, team_id).await.unwrap(),
		"the owner has the app"
	);
	let (status, headers, _) = get_html(&app, &page, Some(&mem_cookie)).await;
	assert!(status.is_redirection(), "{status}");
	assert_eq!(headers[header::LOCATION].to_str().unwrap(), account);
	let (status, _, blocked) = get_html(&app, &account, Some(&mem_cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert!(
		blocked.contains("cannot open that team"),
		"{}",
		page_main(&blocked)
	);
	let (status, body) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&mem_token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
	assert!(body.to_string().contains("two-step"), "{body}");
	// A mutation is refused too, whatever the member's role allows.
	let (_, _, _) = post_signals(
		&app,
		&format!("{page}/policy"),
		Some(&mem_cookie),
		&json!({ "require": false, "grace": "7" }),
	)
	.await;
	assert!(policy::get(&db, own, team_id).await.unwrap().required);
	// Their other team is untouched.
	let (status, _, _) = get_html(
		&app,
		&format!("/teams/{mem_team}/settings/authentication"),
		Some(&mem_cookie),
	)
	.await;
	assert_eq!(status, StatusCode::OK);

	// The owner is not shut out.
	let (status, _, _) = get_html(&app, &page, Some(&own_cookie)).await;
	assert_eq!(status, StatusCode::OK);

	// Setting up the app lets the member back in.
	enrol(&st, mem).await;
	assert_eq!(
		policy::standing(&db, mem, team_id).await.unwrap(),
		policy::Standing::Fine
	);
	let (status, _, _) = get_html(&app, &page, Some(&mem_cookie)).await;
	assert_eq!(status, StatusCode::OK);

	// Another team's policy is not readable or changeable from outside: 404, and a plain
	// member may not change it.
	assert!(policy::get(&db, out, team_id).await.is_err());
	let (status, _, _) = get_html(&app, &page, Some(&out_cookie)).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let _ = post_signals(
		&app,
		&format!("/teams/{team}/settings/authentication/policy"),
		Some(&out_cookie),
		&json!({ "require": false, "grace": "7" }),
	)
	.await;
	let _ = post_signals(
		&app,
		&format!("/teams/{team}/settings/authentication/policy"),
		Some(&mem_cookie),
		&json!({ "require": false, "grace": "7" }),
	)
	.await;
	assert!(
		policy::get(&db, own, team_id).await.unwrap().required,
		"neither an outsider nor a plain member could switch it off"
	);
	assert!(policy::set(&db, mem, team_id, false, 0).await.is_err());
	assert!(policy::set(&db, out, team_id, false, 0).await.is_err());
	let _ = out_team;

	// Switching it off lifts everything for someone without the app.
	policy::set(&db, own, team_id, false, 7).await.unwrap();
	assert_eq!(
		policy::standing(&db, out, TeamId(out_team)).await.unwrap(),
		policy::Standing::Fine
	);
	assert!(audit_actions(&db).await.iter().any(|a| a == "team.update"));
}

// --- sessions --------------------------------------------------------------------------

#[db_test]
async fn two_factor_sessions_are_listed_and_revoked_by_their_owner_only(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	let (_, team) = user_with_team(&app, "ann").await;
	user_with_team(&app, "bob").await;
	let ann = uid(&db, "ann").await;
	let bob = uid(&db, "bob").await;

	let sign_in = |name: &'static str| {
		let app = app.clone();
		async move {
			let mut b = Browser::default();
			b.post(&app, "/login", &creds(name)).await;
			assert!(b.has_session());
			b
		}
	};
	let mut one = sign_in("ann").await;
	let mut two = sign_in("ann").await;
	let mut three = sign_in("ann").await;
	let mut bobs = sign_in("bob").await;
	let page = format!("/teams/{team}/account/security");

	let (status, _, html) = one.get(&app, &page).await;
	assert_eq!(status, StatusCode::OK);
	assert!(html.contains("This device"));
	assert_eq!(html.matches("This device").count(), 1);

	let token = one.jar["session"].clone();
	let list = crate::auth::session::list(&db, ann, &token).await.unwrap();
	// three sign-ins here plus the one that registered the account
	assert_eq!(list.len(), 4);
	assert_eq!(list.iter().filter(|e| e.current).count(), 1);
	let other = list.iter().find(|e| !e.current).unwrap().id;

	// Bob's session is not Ann's to end: a 404, and it still works.
	let bobs_id = crate::auth::session::list(&db, bob, &bobs.jar["session"])
		.await
		.unwrap()[0]
		.id;
	let refused = crate::auth::service::revoke_session(&db, ann, bobs_id).await;
	assert!(matches!(refused, Err(ServerError::NotFound)));
	let _ = one
		.post(
			&app,
			&format!("{page}/sessions/{bobs_id}/revoke"),
			&json!({}),
		)
		.await;
	let (status, _, _) = bobs
		.get(
			&app,
			&format!(
				"/teams/{}/account/security",
				crate::tests::team_id(&db, "bob").await
			),
		)
		.await;
	assert_eq!(status, StatusCode::OK, "bob is still signed in");

	// Ann ends one of her own: that browser is signed out, the others are not.
	let (status, _, _) = one
		.post(&app, &format!("{page}/sessions/{other}/revoke"), &json!({}))
		.await;
	assert_eq!(status, StatusCode::OK);
	let after = crate::auth::session::list(&db, ann, &token).await.unwrap();
	assert_eq!(after.len(), 3);
	assert!(after.iter().all(|e| e.id != other));
	let mut gone = 0;
	for b in [&mut two, &mut three] {
		let (status, _, _) = b.get(&app, &page).await;
		if status != StatusCode::OK {
			gone += 1;
			assert!(
				status.is_redirection() || status == StatusCode::UNAUTHORIZED,
				"{status}"
			);
		}
	}
	assert_eq!(gone, 1, "exactly the revoked browser lost its session");

	// "Sign out everywhere else" leaves only this one.
	let (status, _, _) = one
		.post(&app, &format!("{page}/sessions/revoke-others"), &json!({}))
		.await;
	assert_eq!(status, StatusCode::OK);
	let rest = crate::auth::session::list(&db, ann, &token).await.unwrap();
	assert_eq!(rest.len(), 1);
	assert!(rest[0].current);
	let (status, _, _) = one.get(&app, &page).await;
	assert_eq!(status, StatusCode::OK);

	let actions = audit_actions(&db).await;
	assert!(actions.iter().filter(|a| *a == "session.login").count() >= 4);
	assert!(actions.iter().any(|a| a == "session.revoke"), "{actions:?}");
}

// --- passkeys --------------------------------------------------------------------------

async fn json_post(
	app: &Router,
	uri: &str,
	cookie: Option<&str>,
	body: &Value,
) -> (StatusCode, Value) {
	let (status, _, text) = post_signals(app, uri, cookie, body).await;
	(status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[db_test]
async fn two_factor_passkeys_register_and_sign_in_with_a_soft_authenticator(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	let (_, team) = user_with_team(&app, "ann").await;
	let ann = uid(&db, "ann").await;
	let cookie = crate::ui::testing::ui_login(&app, "ann").await;
	let origin = Url::parse(ORIGIN).unwrap();
	let mut key = SoftPasskey::new(true);

	// Registration: the server's options go to the authenticator, its answer comes back.
	let (status, start) =
		json_post(&app, "/account/passkeys/start", Some(&cookie), &json!({})).await;
	assert_eq!(status, StatusCode::OK, "{start}");
	let options = serde_json::from_value(start["options"].clone()).unwrap();
	let credential = key.do_registration(origin.clone(), options).unwrap();
	let (status, done) = json_post(
		&app,
		"/account/passkeys/finish",
		Some(&cookie),
		&json!({ "flow": start["flow"], "name": "Soft key", "credential": credential }),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{done}");
	assert_eq!(
		done["codes"].as_array().unwrap().len(),
		RECOVERY_CODES,
		"the first second factor brings the recovery codes"
	);
	let listed = passkeys::list(&db, ann).await.unwrap();
	assert_eq!(listed.len(), 1);
	assert_eq!(listed[0].name, "Soft key");
	// A challenge is good for one registration.
	let (status, _) = json_post(
		&app,
		"/account/passkeys/finish",
		Some(&cookie),
		&json!({ "flow": start["flow"], "name": "Again", "credential": credential }),
	)
	.await;
	assert!(status.is_client_error());
	assert_eq!(passkeys::list(&db, ann).await.unwrap().len(), 1);
	// The credential is stored as data, not as the secret the authenticator keeps.
	let stored: Value = sqlx::query_scalar("SELECT credential FROM passkeys")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(stored.get("cred").is_some());

	// A passkey counts as a second factor for the team policy.
	assert!(service::enrolled(&db, ann).await.unwrap());
	policy::set(&db, ann, TeamId(team), true, 7).await.unwrap();
	assert_eq!(
		policy::standing(&db, ann, TeamId(team)).await.unwrap(),
		policy::Standing::Fine
	);

	// The password no longer opens a session on its own; the passkey finishes it.
	let mut b = Browser::default();
	b.post(&app, "/login", &creds("ann")).await;
	assert!(!b.has_session() && b.jar.contains_key("pk_2fa"));
	let cookie2 = b.cookie();
	let (status, start) = json_post(
		&app,
		"/login/two-step/passkey/start",
		cookie2.as_deref(),
		&json!({}),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{start}");
	let options = serde_json::from_value(start["options"].clone()).unwrap();
	let assertion = key.do_authentication(origin.clone(), options).unwrap();
	let (status, headers, text) = send(
		&app,
		"POST",
		"/login/two-step/passkey/finish",
		cookie2.as_deref(),
		Some(&json!({ "flow": start["flow"], "credential": assertion }).to_string()),
		Some("http://podkit.test"),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{text}");
	assert!(
		headers
			.get_all(header::SET_COOKIE)
			.iter()
			.any(|c| c.to_str().unwrap().starts_with("session=")),
		"the second step opens the session"
	);

	// The same assertion cannot be played again.
	let (status, _) = json_post(
		&app,
		"/login/two-step/passkey/finish",
		cookie2.as_deref(),
		&json!({ "flow": start["flow"], "credential": assertion }),
	)
	.await;
	assert!(status.is_client_error(), "{status}");

	// No password at all: the email picks the passkeys, the authenticator signs.
	let (status, start) = json_post(
		&app,
		"/login/passkey/start",
		None,
		&json!({ "email": "ann@example.com" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{start}");
	let options = serde_json::from_value(start["options"].clone()).unwrap();
	let assertion = key.do_authentication(origin.clone(), options).unwrap();
	let (status, headers, text) = post_signals(
		&app,
		"/login/passkey/finish",
		None,
		&json!({ "flow": start["flow"], "credential": assertion }),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{text}");
	assert!(
		headers
			.get_all(header::SET_COOKIE)
			.iter()
			.any(|c| c.to_str().unwrap().starts_with("session=")),
		"a session is opened"
	);
	let passkey_sessions: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE method = 'passkey'")
			.fetch_one(&db)
			.await
			.unwrap();
	assert_eq!(passkey_sessions, 1);
	assert!(
		passkeys::list(&db, ann).await.unwrap()[0]
			.last_used_at
			.is_some()
	);

	// Unknown email and no passkey look the same, so accounts cannot be probed.
	user_with_team(&app, "bob").await;
	let (s1, a) = json_post(
		&app,
		"/login/passkey/start",
		None,
		&json!({ "email": "bob@example.com" }),
	)
	.await;
	let (s2, b2) = json_post(
		&app,
		"/login/passkey/start",
		None,
		&json!({ "email": "nobody@example.com" }),
	)
	.await;
	assert_eq!(s1, s2);
	assert_eq!(a, b2);

	// A forged answer is refused: another authenticator, same challenge.
	let (_, start) = json_post(
		&app,
		"/login/passkey/start",
		None,
		&json!({ "email": "ann@example.com" }),
	)
	.await;
	let options: webauthn_rs::prelude::RequestChallengeResponse =
		serde_json::from_value(start["options"].clone()).unwrap();
	let mut stranger = SoftPasskey::new(true);
	assert!(stranger.do_authentication(origin, options).is_err());
	let (status, _) = json_post(
		&app,
		"/login/passkey/finish",
		None,
		&json!({ "flow": start["flow"], "credential": {
			"id": "AAAA", "rawId": "AAAA", "type": "public-key",
			"response": { "authenticatorData": "AAAA", "clientDataJSON": "AAAA", "signature": "AAAA" },
			"extensions": {},
		} }),
	)
	.await;
	assert!(status.is_client_error());

	// Removing it asks for the password and takes the recovery codes with it.
	let id = listed[0].id;
	assert!(
		passkeys::remove(&db, ann, id, "wrong password")
			.await
			.is_err()
	);
	assert_eq!(passkeys::list(&db, ann).await.unwrap().len(), 1);
	// Not someone else's.
	let bob = uid(&db, "bob").await;
	assert!(matches!(
		passkeys::remove(&db, bob, id, PASSWORD).await,
		Err(ServerError::NotFound)
	));
	passkeys::remove(&db, ann, id, PASSWORD).await.unwrap();
	assert!(passkeys::list(&db, ann).await.unwrap().is_empty());
	assert_eq!(count(&db, "recovery_codes").await, 0);
	assert!(!service::enrolled(&db, ann).await.unwrap());
	let _ = st;
}

#[db_test]
async fn two_factor_recovery_codes_and_secrets_never_reach_a_page_twice(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	let (cookie, team) = ui_session(&app, "ann").await;
	let ann = uid(&db, "ann").await;
	let (secret, recovery) = enrol(&st, ann).await;

	let (status, _, page) = get_html(
		&app,
		&format!("/teams/{team}/account/security"),
		Some(&cookie),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	for text in std::iter::once(&secret).chain(recovery.iter()) {
		assert!(
			!page.contains(text.as_str()),
			"{text} is shown on a later visit"
		);
	}
	// The app's secret is stored sealed.
	let sealed: Vec<u8> = sqlx::query_scalar("SELECT secret FROM totp_credentials")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&sealed).contains(&secret));

	// Another person's Security page is not reachable through this team.
	let (other_cookie, _) = ui_session(&app, "eve").await;
	let (status, _, _) = get_html(
		&app,
		&format!("/teams/{team}/account/security"),
		Some(&other_cookie),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _, _) = post_signals(
		&app,
		&format!("/teams/{team}/account/security/totp/disable"),
		Some(&other_cookie),
		&json!({ "pw": PASSWORD }),
	)
	.await;
	assert!(status == StatusCode::NOT_FOUND || status == StatusCode::OK);
	assert!(
		service::enrolled(&db, ann).await.unwrap(),
		"eve could not turn Ann's app off"
	);
}

/// A passkey saved by webauthn-rs 0.5.4 (OpenSSL) still loads and serialises to the same JSON in
/// webauthn-rs 0.6 (RustCrypto): stored credentials survive the upgrade.
#[test]
fn two_factor_passkey_saved_by_webauthn_rs_0_5_loads_unchanged() {
	let stored = json!({
		"cred": {
			"cred_id": "87Q8EPkrnz-qtL1qKxlYoJytnfidzIzODFKbNjntocM",
			"cred": {
				"type_": "ES256",
				"key": { "EC_EC2": {
					"curve": "SECP256R1",
					"x": "udJX-Kj1RBOkoz-ZeKMylfycLQjO_kILdnH728gzUpI",
					"y": "BERKFdokLWefcjGoDfzc0VlcLWDaI4rJWy3BWX338Yo"
				} }
			},
			"counter": 0,
			"transports": null,
			"user_verified": true,
			"backup_eligible": false,
			"backup_state": false,
			"registration_policy": "required",
			"extensions": {
				"cred_protect": "Ignored",
				"hmac_create_secret": "NotRequested",
				"appid": "NotRequested",
				"cred_props": "Ignored"
			},
			"attestation": { "data": "Self_", "metadata": "None" },
			"attestation_format": "packed"
		}
	});
	let key: webauthn_rs::prelude::Passkey = serde_json::from_value(stored.clone()).unwrap();
	assert_eq!(key.cred_id().len(), 32);
	assert_eq!(serde_json::to_value(&key).unwrap(), stored);
}
