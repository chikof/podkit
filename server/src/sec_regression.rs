//! Regression tests for the findings of `SECURITY_REVIEW.md` (task `t_363c7e98`).
//!
//! Each test states what the code must do once its finding is fixed, so a fix card has a
//! target. A test still marked `#[ignore]` belongs to a finding nobody has fixed yet: it
//! fails today, and its fix un-ignores it. SEC-01 (`throttle`) and SEC-02 (`net::outbound`) are
//! fixed, so their tests run in the normal gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::any;
use serde_json::json;
use time::OffsetDateTime;
use tokio::net::TcpListener;

use crate::ids::{TeamId, UserId};
use crate::notifications::channel::{ChannelFields, ChannelKind};
use crate::notifications::service as notifications;
use crate::state::AppState;
use crate::tests::{PgPool, call, db_test, state, user_id, user_with_team};
use crate::two_factor::{service as two_factor, totp};

const PASSWORD: &str = "correct horse battery";

async fn world(db: PgPool) -> (AppState, Router) {
	let st = state(db);
	let app = crate::routes::routes(st.clone()).await.unwrap();
	(st, app)
}

fn unix() -> u64 {
	u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap()
}

/// The code the authenticator app shows `offset` 30-second steps from now.
fn code(secret_b32: &str, offset: i64) -> String {
	let secret = totp::base32_decode(secret_b32).unwrap();
	let step = totp::step_of(unix()).checked_add_signed(offset).unwrap();
	format!("{:06}", totp::code_at(&secret, step, totp::DIGITS))
}

// ---- SEC-01: no throttling of password or second-factor guesses -----------------------

/// How many wrong guesses the tests make in a row before the right one. A fix may lock after
/// 5 or 10; 25 is past any sane threshold and still quick in a debug build.
const GUESSES: usize = 25;

/// SEC-01 (high): `POST /api/v1/auth/login` takes a TOTP `code` and nothing counts the wrong
/// ones. (`auth/flow.rs` `MAX_ATTEMPTS` guards one web flow only, and a new flow is one
/// correct password away.) With the password known, a 6-digit code falls to enough requests.
/// Expected after the fix: after a handful of wrong codes the account (or the source) is
/// locked for a while, and even the right code is refused (429 or 401).
#[db_test]
async fn sec_regression_sec01_wrong_totp_codes_lock_the_login(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	user_with_team(&app, "ann").await;
	let ann = UserId(user_id(&db, "ann").await);
	let setup = two_factor::begin_totp(&st, ann).await.unwrap();
	two_factor::confirm_totp(&st, ann, &code(&setup.secret, -1))
		.await
		.unwrap();

	let valid: Vec<String> = (-1..=1).map(|o| code(&setup.secret, o)).collect();
	let login = |code: String| {
		let app = app.clone();
		async move {
			call(
				&app,
				"POST",
				"/api/v1/auth/login",
				None,
				Some(json!({ "email": "ann@example.com", "password": PASSWORD, "code": code })),
			)
			.await
			.0
		}
	};
	let mut tried = 0;
	let mut n = 0_u32;
	while tried < GUESSES {
		n += 1;
		let wrong = format!("{:06}", 100_000 + n);
		if valid.contains(&wrong) {
			continue;
		}
		// Wrong, or refused because the account is waiting: never a session.
		let status = login(wrong).await;
		assert!(
			status == StatusCode::UNAUTHORIZED || status == StatusCode::TOO_MANY_REQUESTS,
			"{status}"
		);
		tried += 1;
	}
	let status = login(code(&setup.secret, 0)).await;
	assert_ne!(
		status,
		StatusCode::OK,
		"the right code was accepted after {GUESSES} wrong ones: nothing limits guesses"
	);
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

/// SEC-01 (high), the password half: wrong passwords for a known email are not counted
/// either, so the account's password can be guessed at the speed of the hash check.
#[db_test]
async fn sec_regression_sec01_wrong_passwords_lock_the_login(db: PgPool) {
	let (_st, app) = world(db).await;
	user_with_team(&app, "bob").await;
	let login = |password: String| {
		let app = app.clone();
		async move {
			call(
				&app,
				"POST",
				"/api/v1/auth/login",
				None,
				Some(json!({ "email": "bob@example.com", "password": password })),
			)
			.await
			.0
		}
	};
	for i in 0..GUESSES {
		let status = login(format!("wrong password {i}")).await;
		assert!(
			status == StatusCode::UNAUTHORIZED || status == StatusCode::TOO_MANY_REQUESTS,
			"{status}"
		);
	}
	let status = login(PASSWORD.to_owned()).await;
	assert_ne!(
		status,
		StatusCode::OK,
		"the right password was accepted after {GUESSES} wrong ones: nothing limits guesses"
	);
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

// ---- SEC-02: notification channels can reach (and read from) internal addresses -------

#[derive(Clone)]
struct Seen(Arc<AtomicUsize>);

/// An HTTP server on loopback that counts requests and answers `status` with `body`, or a
/// redirect to `location`.
async fn stub(status: StatusCode, body: &'static str, location: Option<String>) -> (String, Seen) {
	let seen = Seen(Arc::new(AtomicUsize::new(0)));
	let handler = move |State(seen): State<Seen>, _h: HeaderMap| {
		let location = location.clone();
		async move {
			seen.0.fetch_add(1, Ordering::SeqCst);
			let mut headers = HeaderMap::new();
			if let Some(l) = location {
				headers.insert(header::LOCATION, l.parse().unwrap());
			}
			(status, headers, body)
		}
	};
	let app = Router::new()
		.fallback(any(handler))
		.with_state(seen.clone());
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	(url, seen)
}

struct World {
	st: AppState,
	user: UserId,
	team_id: TeamId,
}

async fn world_of_alice(db: PgPool) -> World {
	let (st, app) = world(db.clone()).await;
	// The stubs of these tests are on 127.0.0.1, which the real policy never reaches: that is
	// what they check. (The ordinary test state allows it, for the other suites' stubs.)
	st.outbound.loopback_ok(false);
	let (_token, team) = user_with_team(&app, "alice").await;
	World {
		user: UserId(user_id(&db, "alice").await),
		team_id: TeamId(team),
		st,
	}
}

fn hook(url: &str) -> ChannelFields {
	ChannelFields {
		url: url.to_owned(),
		signing_secret: "signing-secret-1".to_owned(),
		..ChannelFields::default()
	}
}

/// SEC-02 (high): a webhook channel accepts a loopback / private / link-local address, and
/// `POST`s to it. `notifications/channel.rs` `parse_url` only checks the scheme and a host.
/// Any member who may `create notification` can aim the server at its own host's services,
/// the LAN or the cloud metadata address. Expected after the fix: refused when the channel is
/// made or edited, unless an administrator allowed internal addresses (an instance setting).
#[db_test]
async fn sec_regression_sec02_a_webhook_channel_cannot_target_loopback(db: PgPool) {
	let t = world_of_alice(db).await;
	let (url, seen) = stub(StatusCode::OK, "ok", None).await;
	for target in [
		url.as_str(),
		"http://127.0.0.1:2375/containers/prune",
		"http://169.254.169.254/latest/meta-data/",
		"http://10.0.0.5/admin",
		"http://[::1]:8080/",
	] {
		let made = notifications::create_channel(
			&t.st,
			t.user,
			t.team_id,
			"internal",
			ChannelKind::Webhook,
			&hook(target),
		)
		.await;
		assert!(made.is_err(), "a channel for {target} was accepted");
	}
	assert_eq!(seen.0.load(Ordering::SeqCst), 0, "the stub was contacted");
}

/// SEC-02 (high): the test button returns the first 200 characters of what the target
/// answered (`notifications/send.rs` `post_bytes`), so a non-2xx answer from an internal
/// service is readable by the member who made the channel. Expected after the fix: the
/// message holds the status only, never the body.
#[db_test]
async fn sec_regression_sec02_the_test_button_does_not_echo_the_response_body(db: PgPool) {
	let t = world_of_alice(db).await;
	let (url, _seen) = stub(StatusCode::FORBIDDEN, "INTERNAL-ONLY-SECRET-BODY", None).await;
	// Whatever the fix does about the address, the body must not come back. A fix that
	// refuses loopback here also passes: there is then no outcome to read.
	let Ok(channel) = notifications::create_channel(
		&t.st,
		t.user,
		t.team_id,
		"leak",
		ChannelKind::Webhook,
		&hook(&url),
	)
	.await
	else {
		return;
	};
	let outcome = notifications::test_channel(&t.st, t.user, t.team_id, channel.id)
		.await
		.unwrap();
	assert!(
		!outcome.message.contains("INTERNAL-ONLY-SECRET-BODY"),
		"the response body is shown to the user: {}",
		outcome.message
	);
}

/// SEC-02 (high): redirects are followed (`state.http` is `reqwest::Client::new()`), so even a
/// check of the address when the channel is saved is bypassed by a public URL that answers
/// `302 Location: http://127.0.0.1:...`. Expected after the fix: no redirect is followed when
/// sending a notification.
#[db_test]
async fn sec_regression_sec02_notifications_do_not_follow_redirects(db: PgPool) {
	let t = world_of_alice(db).await;
	let (target, reached) = stub(StatusCode::OK, "ok", None).await;
	let (front, _) = stub(StatusCode::FOUND, "", Some(format!("{target}/inner"))).await;
	if let Ok(channel) = notifications::create_channel(
		&t.st,
		t.user,
		t.team_id,
		"bounce",
		ChannelKind::Webhook,
		&hook(&front),
	)
	.await
	{
		let _ = notifications::test_channel(&t.st, t.user, t.team_id, channel.id).await;
	}
	assert_eq!(
		reached.0.load(Ordering::SeqCst),
		0,
		"the redirect target was requested"
	);
}
