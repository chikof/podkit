//! Sign-in throttling (SEC-01): the limiter itself with a clock we move, then every place a
//! guess can be made. `cargo test -p server --all-features -- throttle`

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

use super::{
	ACCOUNT_LIMIT, Who, begin, begin_at, clear_account, describe, locked_users, wait_after,
};
use crate::error::ServerError;
use crate::ids::{TeamId, UserId};
use crate::state::AppState;
use crate::tests::{PgPool, call, count, db_test, state, user_id, user_with_team};
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

fn code(secret_b32: &str, offset: i64) -> String {
	let secret = totp::base32_decode(secret_b32).unwrap();
	let step = totp::step_of(unix()).checked_add_signed(offset).unwrap();
	format!("{:06}", totp::code_at(&secret, step, totp::DIGITS))
}

async fn login(
	app: &Router,
	email: &str,
	password: &str,
	code: Option<&str>,
) -> (StatusCode, Value) {
	let mut body = json!({ "email": email, "password": password });
	if let Some(code) = code {
		body["code"] = json!(code);
	}
	call(app, "POST", "/api/v1/auth/login", None, Some(body)).await
}

/// An account with the authenticator app on. Returns its secret.
async fn with_totp(st: &AppState, app: &Router, name: &str) -> (UserId, String, Vec<String>) {
	user_with_team(app, name).await;
	let user = UserId(user_id(&st.db, name).await);
	let setup = two_factor::begin_totp(st, user).await.unwrap();
	let recovery = two_factor::confirm_totp(st, user, &code(&setup.secret, -1))
		.await
		.unwrap();
	(user, setup.secret, recovery)
}

/// Makes a lock last. The wait after the fifth failure is one second, which a loaded machine
/// can outrun between two requests of a test; doubling the rows makes it half a minute.
pub(crate) async fn hold(db: &PgPool) {
	sqlx::query("INSERT INTO auth_failures (subject, at) SELECT subject, NOW() FROM auth_failures")
		.execute(db)
		.await
		.unwrap();
}

fn is_locked(e: &ServerError) -> Option<u64> {
	match e {
		ServerError::TooManyAttempts { retry_after } => Some(*retry_after),
		_ => None,
	}
}

#[test]
fn throttle_wait_starts_at_one_second_doubles_and_stops_at_fifteen_minutes() {
	assert_eq!(wait_after(0, 5), 0);
	assert_eq!(wait_after(4, 5), 0);
	assert_eq!(wait_after(5, 5), 1);
	assert_eq!(wait_after(6, 5), 2);
	assert_eq!(wait_after(7, 5), 4);
	assert_eq!(wait_after(14, 5), 512);
	assert_eq!(wait_after(15, 5), 900);
	assert_eq!(wait_after(500, 5), 900);
	assert_eq!(describe(1), "1 second");
	assert_eq!(describe(45), "45 seconds");
	assert_eq!(describe(61), "2 minutes");
	assert_eq!(describe(900), "15 minutes");
	assert_eq!(
		ServerError::TooManyAttempts { retry_after: 600 }.to_string(),
		"Too many attempts, try again in 10 minutes."
	);
}

#[db_test]
async fn throttle_the_wait_after_the_limit_grows_and_a_wrong_guess_is_never_free(db: PgPool) {
	let who = || Who::User(UserId(7));
	let ip = || "10.1.1.1".to_owned();
	let t0 = OffsetDateTime::now_utc();

	for _ in 0..ACCOUNT_LIMIT {
		begin_at(&db, who(), ip(), t0)
			.await
			.unwrap()
			.fail_at(t0)
			.await;
	}
	// The sixth is refused outright, whatever it would have been.
	let wait = begin_at(&db, who(), ip(), t0)
		.await
		.err()
		.and_then(|e| is_locked(&e));
	assert_eq!(wait, Some(1));
	// A refused request is not counted: asking again changes nothing.
	let again = begin_at(&db, who(), ip(), t0 + Duration::milliseconds(200))
		.await
		.err()
		.and_then(|e| is_locked(&e));
	assert_eq!(again, Some(1));

	// Each failure after the wait doubles it.
	let mut at = t0;
	for expected in [2, 4, 8, 16] {
		at += Duration::seconds(expected / 2);
		begin_at(&db, who(), ip(), at)
			.await
			.unwrap()
			.fail_at(at)
			.await;
		let wait = begin_at(&db, who(), ip(), at)
			.await
			.err()
			.and_then(|e| is_locked(&e));
		assert_eq!(wait, Some(u64::try_from(expected).unwrap()));
	}

	// Past the window everything is forgotten.
	let later = at + super::WINDOW + Duration::seconds(1);
	begin_at(&db, who(), ip(), later).await.unwrap().ok().await;
}

#[db_test]
async fn throttle_the_wait_never_passes_fifteen_minutes(db: PgPool) {
	let t0 = OffsetDateTime::now_utc();
	let mut at = t0;
	let mut longest = 0;
	for _ in 0..40 {
		match begin_at(&db, Who::User(UserId(3)), "10.1.1.2".to_owned(), at).await {
			Ok(a) => {
				a.fail_at(at).await;
				at += Duration::seconds(1);
			}
			Err(e) => {
				let wait = is_locked(&e).unwrap();
				longest = longest.max(wait);
				at += Duration::seconds(i64::try_from(wait).unwrap());
			}
		}
	}
	assert!(longest <= 900, "{longest}");
	assert!(longest > 60, "the backoff grew: {longest}");
}

#[db_test]
async fn throttle_a_right_guess_gives_its_row_back_and_a_sign_in_resets_the_account(db: PgPool) {
	let t0 = OffsetDateTime::now_utc();
	let who = || Who::User(UserId(9));
	for _ in 0..ACCOUNT_LIMIT - 1 {
		begin_at(&db, who(), "10.2.2.2".to_owned(), t0)
			.await
			.unwrap()
			.fail_at(t0)
			.await;
	}
	begin_at(&db, who(), "10.2.2.2".to_owned(), t0)
		.await
		.unwrap()
		.ok()
		.await;
	// The right one did not count against the account: the fifth wrong one is still allowed.
	begin_at(&db, who(), "10.2.2.2".to_owned(), t0)
		.await
		.unwrap()
		.fail_at(t0)
		.await;
	assert!(
		begin_at(&db, who(), "10.2.2.2".to_owned(), t0)
			.await
			.is_err()
	);
	// A full sign-in resets the account, and a different address starts clean too.
	clear_account(&db, UserId(9)).await;
	begin_at(&db, who(), "10.2.2.3".to_owned(), t0)
		.await
		.unwrap()
		.fail_at(t0)
		.await;
	// Five rows of the first address stay (they are its own), plus the new guess on each.
	assert_eq!(count(&db, "auth_failures").await, ACCOUNT_LIMIT + 2);
}

#[db_test]
async fn throttle_one_address_spraying_many_accounts_is_stopped_by_the_address_limit(db: PgPool) {
	let t0 = OffsetDateTime::now_utc();
	let ip = "203.0.113.9".to_owned();
	for i in 0..super::IP_LIMIT {
		// Every guess is at a different account, so no account ever reaches its limit.
		begin_at(&db, Who::User(UserId(100 + i)), ip.clone(), t0)
			.await
			.unwrap()
			.fail_at(t0)
			.await;
	}
	let err = begin_at(&db, Who::User(UserId(999)), ip.clone(), t0)
		.await
		.err()
		.expect("the address has to wait");
	assert!(is_locked(&err).is_some());
	// Another address is not affected.
	begin_at(&db, Who::User(UserId(999)), "203.0.113.10".to_owned(), t0)
		.await
		.unwrap()
		.ok()
		.await;
}

#[db_test]
async fn throttle_counters_live_in_postgres_and_survive_a_restart(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	user_with_team(&app, "ann").await;
	for _ in 0..ACCOUNT_LIMIT {
		let (status, _) = login(&app, "ann@example.com", "nope nope nope", None).await;
		assert_eq!(status, StatusCode::UNAUTHORIZED);
	}
	assert!(count(&db, "auth_failures").await >= ACCOUNT_LIMIT);

	// A new process over the same database: nothing is held in memory.
	let (_st2, app2) = world(db.clone()).await;
	hold(&db).await;
	let (status, body) = login(&app2, "ann@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
}

// --- each place a guess can be made ------------------------------------------------------

#[db_test]
async fn throttle_password_login_locks_even_the_right_password_and_says_when_to_retry(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	user_with_team(&app, "bob").await;
	for _ in 0..ACCOUNT_LIMIT {
		let (status, _) = login(&app, "bob@example.com", "wrong password", None).await;
		assert_eq!(status, StatusCode::UNAUTHORIZED);
	}
	hold(&db).await;
	let (status, body) = login(&app, "bob@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
	let message = body["error"].as_str().unwrap();
	assert!(
		message.starts_with("Too many attempts, try again in "),
		"{message}"
	);
	assert!(message.contains("second"), "{message}");

	// After the wait the right password signs in, and that clears the account.
	sqlx::query("UPDATE auth_failures SET at = at - interval '1 minute'")
		.execute(&db)
		.await
		.unwrap();
	let (status, body) = login(&app, "bob@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	let bob = user_id(&db, "bob").await;
	let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_failures WHERE subject = $1")
		.bind(format!("u:{bob}"))
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(left, 0, "a sign-in resets the account's counter");
}

#[db_test]
async fn throttle_success_in_between_resets_the_count(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	user_with_team(&app, "bob").await;
	for round in 0..3 {
		for _ in 0..ACCOUNT_LIMIT - 1 {
			let (status, _) = login(&app, "bob@example.com", "wrong password", None).await;
			assert_eq!(status, StatusCode::UNAUTHORIZED, "round {round}");
		}
		let (status, body) = login(&app, "bob@example.com", PASSWORD, None).await;
		assert_eq!(status, StatusCode::OK, "round {round}: {body}");
	}
}

#[db_test]
async fn throttle_an_unknown_email_is_locked_like_a_known_one(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	user_with_team(&app, "bob").await;
	let mut seen = Vec::new();
	for email in ["bob@example.com", "nobody@example.com"] {
		let mut statuses = Vec::new();
		for _ in 0..=ACCOUNT_LIMIT {
			let (status, body) = login(&app, email, "wrong password", None).await;
			statuses.push((
				status,
				body["error"].as_str().unwrap_or_default().to_owned(),
			));
		}
		seen.push(statuses);
	}
	// The same statuses and the same words, step by step: no account is given away.
	assert_eq!(seen[0], seen[1]);
	assert_eq!(seen[0].last().unwrap().0, StatusCode::TOO_MANY_REQUESTS);
	// Nothing about the unknown email reaches the audit log or the notifications.
	assert_eq!(
		count(&db, "notification_events").await,
		1,
		"only bob's lockout"
	);
}

#[db_test]
async fn throttle_totp_and_recovery_guesses_at_the_json_login_lock_the_account(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	let (_ann, secret, recovery) = with_totp(&st, &app, "ann").await;

	// Wrong authenticator codes and wrong recovery codes count together.
	for i in 0..ACCOUNT_LIMIT {
		let guess = if i % 2 == 0 {
			format!("{:06}", 100_001 + i)
		} else {
			"AAAAA-BBBBB-CCCCC-DDDDD".to_owned()
		};
		let (status, _) = login(&app, "ann@example.com", PASSWORD, Some(&guess)).await;
		assert_eq!(status, StatusCode::UNAUTHORIZED, "guess {i}");
	}
	hold(&db).await;
	let (status, body) = login(&app, "ann@example.com", PASSWORD, Some(&code(&secret, 0))).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
	let (status, _) = login(&app, "ann@example.com", PASSWORD, Some(&recovery[0])).await;
	assert_eq!(
		status,
		StatusCode::TOO_MANY_REQUESTS,
		"a recovery code is refused too"
	);

	// Waiting it out, the right code signs in.
	sqlx::query("UPDATE auth_failures SET at = at - interval '1 minute'")
		.execute(&db)
		.await
		.unwrap();
	let (status, body) = login(&app, "ann@example.com", PASSWORD, Some(&code(&secret, 0))).await;
	assert_eq!(status, StatusCode::OK, "{body}");
}

#[db_test]
async fn throttle_a_lockout_writes_an_audit_event_and_notifies_the_teams(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	let (_t, team) = user_with_team(&app, "bob").await;
	// A channel and a rule that wants sign-in lockouts, as an admin would set up.
	let bob = UserId(user_id(&db, "bob").await);
	let channel = crate::notifications::service::create_channel(
		&st,
		bob,
		TeamId(team),
		"ops",
		crate::notifications::channel::ChannelKind::Discord,
		&crate::notifications::channel::ChannelFields {
			url: "https://example.com/hook".to_owned(),
			..Default::default()
		},
	)
	.await
	.unwrap();
	crate::notifications::service::create_rule(
		&db,
		bob,
		TeamId(team),
		crate::notifications::service::RuleInput {
			name: "locks".to_owned(),
			kinds: vec!["sign_in_locked".to_owned()],
			channel_ids: vec![channel.id.0],
			project_id: None,
			environment_id: None,
			min_severity: crate::notifications::Severity::Info,
			enabled: true,
		},
	)
	.await
	.unwrap();

	for _ in 0..ACCOUNT_LIMIT {
		login(&app, "bob@example.com", "wrong password", None).await;
	}

	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events ORDER BY id")
		.fetch_all(&db)
		.await
		.unwrap();
	assert_eq!(
		actions.iter().filter(|a| *a == "session.locked").count(),
		1,
		"{actions:?}"
	);
	let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM notification_events")
		.fetch_all(&db)
		.await
		.unwrap();
	assert_eq!(kinds, ["sign_in_locked"]);
	assert_eq!(count(&db, "notification_deliveries").await, 1);

	// Further wrong guesses while waiting do not repeat it.
	login(&app, "bob@example.com", "wrong password", None).await;
	assert_eq!(count(&db, "notification_events").await, 1);
}

#[db_test]
async fn throttle_an_admin_clears_a_lockout_from_the_members_api(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (_bob_token, _) = user_with_team(&app, "bob").await;
	let bob = UserId(user_id(&db, "bob").await);
	let (status, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(&alice),
		Some(json!({ "email": "bob@example.com", "role_id": 2 })),
	)
	.await;
	assert!(status.is_success());

	for _ in 0..ACCOUNT_LIMIT {
		login(&app, "bob@example.com", "wrong password", None).await;
	}
	hold(&db).await;
	let (status, _) = login(&app, "bob@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	assert!(locked_users(&db, &[bob]).await.unwrap().contains_key(&bob));

	// A plain member cannot clear it; the owner can.
	sqlx::query("DELETE FROM auth_failures")
		.execute(&db)
		.await
		.unwrap();
	let (s, b) = login(&app, "bob@example.com", PASSWORD, None).await;
	assert_eq!(s, StatusCode::OK);
	let bob_session = b["token"].as_str().unwrap().to_owned();
	for _ in 0..ACCOUNT_LIMIT {
		login(&app, "bob@example.com", "wrong password", None).await;
	}
	let unlock = format!("/api/v1/teams/{team}/members/{}/unlock", bob.0);
	let (status, _) = call(&app, "POST", &unlock, Some(&bob_session), None).await;
	assert_eq!(status, StatusCode::FORBIDDEN);
	hold(&db).await;
	let (status, _) = login(&app, "bob@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

	let (status, _) = call(&app, "POST", &unlock, Some(&alice), None).await;
	assert_eq!(status, StatusCode::NO_CONTENT);
	let (status, body) = login(&app, "bob@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events")
		.fetch_all(&db)
		.await
		.unwrap();
	assert!(actions.iter().any(|a| a == "member.unlock"), "{actions:?}");

	// Another team's admin gets a 404-class answer, not a way to unlock strangers.
	let (carol, carol_team) = user_with_team(&app, "carol").await;
	let _ = carol_team;
	let (status, _) = call(&app, "POST", &unlock, Some(&carol), None).await;
	assert!(
		status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
		"{status}"
	);
}

#[db_test]
async fn throttle_the_members_page_shows_the_lockout_and_the_owner_can_unlock_it(db: PgPool) {
	use crate::ui::testing::{
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

#[db_test]
async fn throttle_the_web_sign_in_says_so_in_words(db: PgPool) {
	use crate::ui::testing::{Browser, assert_contains};

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

#[db_test]
async fn throttle_the_web_second_step_counts_codes_across_new_sign_ins(db: PgPool) {
	use crate::ui::testing::Browser;

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

#[db_test]
async fn throttle_the_security_page_password_check_is_counted_too(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	user_with_team(&app, "ann").await;
	let ann = UserId(user_id(&db, "ann").await);
	for _ in 0..ACCOUNT_LIMIT {
		let e = two_factor::confirm_password(&st.db, ann, "wrong")
			.await
			.unwrap_err();
		assert!(matches!(e, ServerError::Validation(_)), "{e}");
	}
	let e = two_factor::confirm_password(&st.db, ann, PASSWORD)
		.await
		.unwrap_err();
	assert!(is_locked(&e).is_some(), "{e}");
}

#[db_test]
async fn throttle_wrong_api_tokens_lock_the_address_not_the_sign_in(db: PgPool) {
	let (_st, app) = world(db.clone()).await;
	let (session, team) = user_with_team(&app, "ann").await;
	let (status, made) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/api-tokens"),
		Some(&session),
		Some(json!({ "name": "ci", "scopes": ["read"] })),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{made}");
	let good = made["secret"].as_str().unwrap().to_owned();
	let (status, _) = call(&app, "GET", "/api/v1/teams", Some(&good), None).await;
	assert_eq!(status, StatusCode::OK);

	let wrong = format!("pk_{}_{}", "a".repeat(8), "b".repeat(64));
	for _ in 0..super::TOKEN_LIMIT {
		let (status, _) = call(&app, "GET", "/api/v1/teams", Some(&wrong), None).await;
		assert_eq!(status, StatusCode::UNAUTHORIZED);
	}
	hold(&db).await;
	let (status, body) = call(&app, "GET", "/api/v1/teams", Some(&wrong), None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
	// Even the right token waits: a guess must not be answered while the address is locked.
	let (status, _) = call(&app, "GET", "/api/v1/teams", Some(&good), None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	// A malformed token is refused like a wrong one.
	let (status, _) = call(&app, "GET", "/api/v1/teams", Some("pk_nonsense"), None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	// Signing in as a person is not blocked by a script's bad token.
	let (status, _) = login(&app, "ann@example.com", PASSWORD, None).await;
	assert_eq!(status, StatusCode::OK);
}

#[db_test]
async fn throttle_begin_without_a_request_still_works(db: PgPool) {
	// Called outside a request (a job, a test): the address is "unknown", not a panic.
	let attempt = begin(&db, Who::User(UserId(1))).await.unwrap();
	attempt.ok().await;
	let unknown: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_failures")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(unknown, 0);
}
