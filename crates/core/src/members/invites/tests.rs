//! Team invites: only a hash is stored, a link works once and only for its address, revoke
//! and expiry end it, permissions and the role rule hold, sending is rate-limited and goes
//! through the team's SMTP relay with a `PUBLIC_URL` link.
//! `cargo test -p podkit-core -- invite`

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::{SENDS_PER_HOUR, well_formed};
use crate::state::Registration;
use crate::tests::{app, app_with_registration, call, count, db_test, member_role, user_with_team};

const OWNER: i64 = 1;
const MEMBER: i64 = 2;

async fn register(app: &Router, name: &str) -> StatusCode {
	let creds = json!({
		"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
	});
	call(app, "POST", "/api/v1/auth/register", None, Some(creds))
		.await
		.0
}

async fn login(app: &Router, name: &str) -> String {
	let creds =
		json!({ "email": format!("{name}@example.com"), "password": "correct horse battery" });
	let (s, body) = call(app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
	assert_eq!(s, StatusCode::OK, "login {name}");
	body["token"].as_str().unwrap().to_owned()
}

async fn invite(
	app: &Router,
	token: &str,
	team: i64,
	email: &str,
	role: i64,
) -> (StatusCode, Value) {
	call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/invites"),
		Some(token),
		Some(json!({ "email": email, "role_id": role })),
	)
	.await
}

/// The token at the end of an invite link.
fn token_of(link: &Value) -> String {
	let link = link.as_str().unwrap();
	link.rsplit('/').next().unwrap().to_owned()
}

fn sign_up_body(email: &str, invite: &str) -> Value {
	json!({ "name": "Newcomer", "email": email, "password": "correct horse battery", "invite": invite })
}

async fn actions(db: &PgPool, team: i64) -> Vec<String> {
	sqlx::query_scalar("SELECT action FROM audit_events WHERE team_id = $1 ORDER BY created_at")
		.bind(team)
		.fetch_all(db)
		.await
		.unwrap()
}

#[db_test]
async fn invite_stores_only_a_hash_and_hands_back_the_link_when_not_emailed(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;

	let (s, body) = invite(&app, &alice, team, "New.Person@Example.COM", MEMBER).await;
	assert_eq!(s, StatusCode::CREATED, "{body}");
	// Stored in lower case: one address, one invite.
	assert_eq!(body["email"], "new.person@example.com");
	assert_eq!(body["emailed"], false);
	// No PUBLIC_URL in tests: nothing can be emailed, the note says why.
	assert!(
		body["note"].as_str().unwrap().contains("PUBLIC_URL"),
		"{body}"
	);
	let link = body["link"].as_str().unwrap();
	assert!(link.starts_with("/invite/"), "{link}");
	let token = token_of(&body["link"]);
	assert!(well_formed(&token));

	let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM team_invites")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(stored, Sha256::digest(token.as_bytes()).to_vec());
	let anywhere: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM team_invites WHERE email LIKE '%' || $1 || '%' OR encode(token_hash, 'hex') = $1",
	)
	.bind(&token)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(anywhere, 0, "the token itself is stored nowhere");

	// The list never carries a link or a token.
	let (s, list) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/invites"),
		Some(&alice),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);
	assert!(!list.to_string().contains(&token));
	assert!(list[0].get("link").is_none());
	assert!(
		actions(&db, team)
			.await
			.contains(&"invite.create".to_owned())
	);
}

#[db_test]
async fn invite_lets_its_address_sign_up_on_a_closed_instance_once(db: PgPool) {
	let open = app(db.clone()).await;
	let (alice, team) = user_with_team(&open, "alice").await;
	let (_, body) = invite(&open, &alice, team, "dee@example.com", MEMBER).await;
	let token = token_of(&body["link"]);

	for policy in [Registration::Closed, Registration::FirstUser] {
		let closed = app_with_registration(db.clone(), policy).await;
		// Without the invite the door is shut.
		assert_eq!(register(&closed, "dee").await, StatusCode::FORBIDDEN);
	}
	let closed = app_with_registration(db.clone(), Registration::Closed).await;

	// The invite is for one address only.
	let (s, body) = call(
		&closed,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("mallory@example.com", &token)),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert_eq!(count(&db, "users").await, 1);

	let (s, body) = call(
		&closed,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("dee@example.com", &token)),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{body}");
	assert_eq!(member_role(&db, team, "Newcomer").await, MEMBER);
	assert_eq!(count(&db, "team_invites").await, 0, "used up");

	// Single use.
	let (s, body) = call(
		&closed,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("dee@example.com", &token)),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
	assert_eq!(
		body["error"],
		"this invite is not valid any more; ask for a new one"
	);

	let seen = actions(&db, team).await;
	assert!(seen.contains(&"invite.accept".to_owned()), "{seen:?}");
}

#[db_test]
async fn invite_for_an_existing_account_is_accepted_after_signing_in(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	assert!(register(&app, "bob").await.is_success());
	assert!(register(&app, "carol").await.is_success());
	let (_, body) = invite(&app, &alice, team, "bob@example.com", MEMBER).await;
	let token = token_of(&body["link"]);

	// Signing up again with the address is refused: it has an account.
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("bob@example.com", &token)),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT);

	// Not signed in: nothing.
	let accept = json!({ "token": token });
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/invites/accept",
		None,
		Some(accept.clone()),
	)
	.await;
	assert!(s.is_client_error(), "{s}");
	assert_eq!(count(&db, "team_members").await, 1);

	// Another account holding the link cannot use it, and the link stays good for bob.
	let carol = login(&app, "carol").await;
	let (s, body) = call(
		&app,
		"POST",
		"/api/v1/auth/invites/accept",
		Some(&carol),
		Some(accept.clone()),
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert_eq!(count(&db, "team_members").await, 1);
	assert_eq!(count(&db, "team_invites").await, 1);

	let bob = login(&app, "bob").await;
	let (s, body) = call(
		&app,
		"POST",
		"/api/v1/auth/invites/accept",
		Some(&bob),
		Some(accept.clone()),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["team_id"], team);
	assert_eq!(member_role(&db, team, "bob").await, MEMBER);

	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/invites/accept",
		Some(&bob),
		Some(accept),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "single use");
}

#[db_test]
async fn invite_revoked_or_expired_stops_working(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (_, body) = invite(&app, &alice, team, "x@example.com", MEMBER).await;
	let revoked = token_of(&body["link"]);
	let id = body["id"].as_i64().unwrap();
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/invites/{id}"),
		Some(&alice),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NO_CONTENT);
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("x@example.com", &revoked)),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert!(
		actions(&db, team)
			.await
			.contains(&"invite.revoke".to_owned())
	);

	let (_, body) = invite(&app, &alice, team, "y@example.com", MEMBER).await;
	let expired = token_of(&body["link"]);
	sqlx::query("UPDATE team_invites SET expires_at = NOW() - INTERVAL '1 minute'")
		.execute(&db)
		.await
		.unwrap();
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("y@example.com", &expired)),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_eq!(count(&db, "users").await, 1);

	// An expired invite makes room for a new one to the same address.
	let (s, _) = invite(&app, &alice, team, "y@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::CREATED);
}

#[db_test]
async fn invite_resend_replaces_the_link(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (_, body) = invite(&app, &alice, team, "z@example.com", MEMBER).await;
	let old = token_of(&body["link"]);
	let id = body["id"].as_i64().unwrap();
	let (s, body) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/invites/{id}/resend"),
		Some(&alice),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let new = token_of(&body["link"]);
	assert_ne!(old, new);
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("z@example.com", &old)),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("z@example.com", &new)),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED);
}

#[db_test]
async fn invite_needs_permission_and_stays_in_its_team(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (zed, zed_team) = user_with_team(&app, "zed").await;
	assert!(register(&app, "bob").await.is_success());
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(&alice),
		Some(json!({ "email": "bob@example.com", "role_id": MEMBER })),
	)
	.await;
	assert!(s.is_success());
	let bob = login(&app, "bob").await;

	// A plain member reads the list but cannot invite, resend or revoke.
	let (s, _) = invite(&app, &bob, team, "q@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (_, body) = invite(&app, &alice, team, "q@example.com", MEMBER).await;
	let id = body["id"].as_i64().unwrap();
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/invites"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/invites/{id}"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);

	// Another team's owner: 404 everywhere, the invite stays.
	let (s, _) = invite(&app, &zed, team, "q2@example.com", OWNER).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	for (method, path) in [
		("GET", format!("/api/v1/teams/{team}/invites")),
		("DELETE", format!("/api/v1/teams/{team}/invites/{id}")),
		("POST", format!("/api/v1/teams/{team}/invites/{id}/resend")),
		// Their own team does not have it either.
		("DELETE", format!("/api/v1/teams/{zed_team}/invites/{id}")),
	] {
		let (s, _) = call(&app, method, &path, Some(&zed), None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	assert_eq!(count(&db, "team_invites").await, 1);

	// Already a member; already invited.
	let (s, _) = invite(&app, &alice, team, "bob@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::CONFLICT);
	let (s, _) = invite(&app, &alice, team, "Q@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::CONFLICT);

	// Another team's role cannot be handed out.
	crate::tests::team_role(&db, 900, zed_team, "Foreign").await;
	let (s, _) = invite(&app, &alice, team, "r@example.com", 900).await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[db_test]
async fn invite_api_tokens_need_root_to_invite(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (s, made) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/api-tokens"),
		Some(&alice),
		Some(json!({ "name": "ci", "scopes": ["read", "write", "delete"] })),
	)
	.await;
	assert!(s.is_success(), "{made}");
	let pk = made["secret"].as_str().unwrap().to_owned();
	let (s, _) = invite(&app, &pk, team, "t@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/invites"),
		Some(&pk),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	// Accepting belongs to a person: tokens never reach `/auth`.
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/invites/accept",
		Some(&pk),
		Some(json!({ "token": "0".repeat(64) })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "team_invites").await, 0);
}

#[db_test]
async fn invite_cannot_hand_out_more_than_the_inviters_role(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	assert!(register(&app, "bob").await.is_success());
	// A team role that may manage memberships and read, nothing else.
	crate::tests::team_role(&db, 901, team, "Recruiter").await;
	sqlx::query(
		"INSERT INTO role_permissions (role_id, permission_id)
		SELECT 901, id FROM permissions WHERE resource = 'membership' OR action = 'read'",
	)
	.execute(&db)
	.await
	.unwrap();
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(&alice),
		Some(json!({ "email": "bob@example.com", "role_id": 901 })),
	)
	.await;
	assert!(s.is_success());
	let bob = login(&app, "bob").await;

	let (s, body) = invite(&app, &bob, team, "o@example.com", OWNER).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert!(body["error"].as_str().unwrap().contains("more than yours"));
	let (s, _) = invite(&app, &bob, team, "m@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::CREATED);
	let (s, _) = invite(&app, &bob, team, "p@example.com", 901).await;
	assert_eq!(s, StatusCode::CREATED);
	assert_eq!(count(&db, "team_invites").await, 2);
}

#[db_test]
async fn invite_sending_is_limited_per_team_per_hour(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	for i in 0..SENDS_PER_HOUR {
		let (s, body) = invite(&app, &alice, team, &format!("u{i}@example.com"), MEMBER).await;
		assert_eq!(s, StatusCode::CREATED, "{i}: {body}");
	}
	let (s, _) = invite(&app, &alice, team, "one-more@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
	// Revoking does not give sends back, and a resend counts too.
	let id: i64 = sqlx::query_scalar("SELECT id FROM team_invites LIMIT 1")
		.fetch_one(&db)
		.await
		.unwrap();
	let (s, _) = call(
		&app,
		"DELETE",
		&format!("/api/v1/teams/{team}/invites/{id}"),
		Some(&alice),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NO_CONTENT);
	let (s, _) = invite(&app, &alice, team, "u0@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
	assert_eq!(
		count(&db, "team_invites").await,
		SENDS_PER_HOUR - 1,
		"nothing made past the limit"
	);
}

/// A minimal SMTP server that keeps the transcript (as in the notifications tests).
async fn smtp_stub() -> (u16, Arc<Mutex<Vec<String>>>) {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let log: Arc<Mutex<Vec<String>>> = Arc::default();
	let kept = log.clone();
	tokio::spawn(async move {
		while let Ok((socket, _)) = listener.accept().await {
			let log = kept.clone();
			tokio::spawn(async move {
				let (read, mut write) = socket.into_split();
				let mut lines = BufReader::new(read).lines();
				write.write_all(b"220 stub ESMTP\r\n").await.unwrap();
				let mut data = false;
				while let Ok(Some(line)) = lines.next_line().await {
					log.lock().unwrap().push(line.clone());
					if data {
						if line == "." {
							data = false;
							write.write_all(b"250 queued\r\n").await.unwrap();
						}
						continue;
					}
					let upper = line.to_ascii_uppercase();
					let reply: &[u8] = if upper.starts_with("EHLO") {
						b"250-stub\r\n250 8BITMIME\r\n"
					} else if upper.starts_with("DATA") {
						data = true;
						b"354 go ahead\r\n"
					} else if upper.starts_with("QUIT") {
						let _ = write.write_all(b"221 bye\r\n").await;
						return;
					} else {
						b"250 OK\r\n"
					};
					write.write_all(reply).await.unwrap();
				}
			});
		}
	});
	(port, log)
}

#[db_test]
async fn invite_is_emailed_through_the_teams_relay_with_a_public_url_link(db: PgPool) {
	let mut st = crate::tests::state(db.clone());
	let mut config = (*st.config).clone();
	config.public_url = Some("https://podkit.example.com".to_owned());
	st.config = Arc::new(config);
	let app = crate::routes::routes(st).await.unwrap();
	let (alice, team) = user_with_team(&app, "alice").await;

	// No relay yet: made, not emailed, and the link is absolute.
	let (s, body) = invite(&app, &alice, team, "first@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::CREATED);
	assert_eq!(body["emailed"], false);
	assert!(body["note"].as_str().unwrap().contains("SMTP"), "{body}");
	assert!(
		body["link"]
			.as_str()
			.unwrap()
			.starts_with("https://podkit.example.com/invite/")
	);

	let (port, log) = smtp_stub().await;
	let (s, _) = call(
		&app,
		"PUT",
		&format!("/api/v1/teams/{team}/notifications/smtp"),
		Some(&alice),
		Some(json!({
			"host": "127.0.0.1", "port": port, "security": "none",
			"username": "", "password": "", "from_addr": "podkit <podkit@example.com>",
		})),
	)
	.await;
	assert!(s.is_success());

	let (s, body) = invite(&app, &alice, team, "second@example.com", MEMBER).await;
	assert_eq!(s, StatusCode::CREATED, "{body}");
	assert_eq!(body["emailed"], true);
	assert!(
		body["link"].is_null(),
		"an emailed link is not shown: {body}"
	);
	// The body may be quoted-printable: undo its soft line breaks before reading the link.
	let transcript = log.lock().unwrap().join("\n").replace("=\n", "");
	assert!(
		transcript.contains("RCPT TO:<second@example.com>"),
		"{transcript}"
	);
	assert!(
		transcript.contains("https://podkit.example.com/invite/"),
		"{transcript}"
	);
	// The link in the mail is the one whose hash is stored.
	let at = transcript.find("/invite/").unwrap() + "/invite/".len();
	let token = &transcript[at..at + 64];
	let stored: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM team_invites WHERE email = 'second@example.com' AND token_hash = $1",
	)
	.bind(Sha256::digest(token.as_bytes()).to_vec())
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(stored, 1);
}

#[test]
fn invite_email_text_keeps_names_on_one_line() {
	let (subject, body) = super::email_text(
		"ops\r\nBcc: evil@example.com",
		"Alice",
		"Member",
		"https://podkit.example.com/invite/abc",
		time::macros::datetime!(2026-12-01 10:00 UTC),
	);
	assert!(
		!subject.contains('\n') && !subject.contains('\r'),
		"{subject}"
	);
	assert!(body.contains("https://podkit.example.com/invite/abc"));
	assert!(body.contains("2026-12-01"));
}

#[test]
fn invite_tokens_must_have_the_shape_podkit_makes() {
	assert!(well_formed(&"a1".repeat(32)));
	assert!(!well_formed(&"A1".repeat(32)));
	assert!(!well_formed(&"a1".repeat(31)));
	assert!(!well_formed(&format!("{}'", "a".repeat(63))));
	assert!(!well_formed(""));
}

#[db_test]
async fn invite_dies_with_its_senders_authority(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	assert!(register(&app, "bob").await.is_success());
	let (s, _) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/members"),
		Some(&alice),
		Some(json!({ "email": "bob@example.com", "role_id": OWNER })),
	)
	.await;
	assert!(s.is_success());
	let bob = login(&app, "bob").await;
	let (_, body) = invite(&app, &bob, team, "kept@example.com", MEMBER).await;
	let kept = token_of(&body["link"]);
	let (_, body) = invite(&app, &bob, team, "dropped@example.com", OWNER).await;
	let dropped = token_of(&body["link"]);

	// A sender whose role no longer covers an invite cannot back it: a link for it is refused
	// even before the list is pruned.
	sqlx::query("UPDATE team_members SET role_id = $1 WHERE user_id = $2")
		.bind(MEMBER)
		.bind(crate::tests::user_id(&db, "bob").await)
		.execute(&db)
		.await
		.unwrap();
	for (email, token) in [
		("kept@example.com", &kept),
		("dropped@example.com", &dropped),
	] {
		let (s, _) = call(
			&app,
			"POST",
			"/api/v1/auth/register",
			None,
			Some(sign_up_body(email, token)),
		)
		.await;
		assert_eq!(
			s,
			StatusCode::NOT_FOUND,
			"{email}: bob lost create membership"
		);
	}
	assert_eq!(count(&db, "users").await, 2);

	// Back to owner through the API, then lowered through the API: the change prunes them.
	let bob_id = crate::tests::user_id(&db, "bob").await;
	for role in [OWNER, MEMBER] {
		let (s, _) = call(
			&app,
			"PATCH",
			&format!("/api/v1/teams/{team}/members/{bob_id}"),
			Some(&alice),
			Some(json!({ "role_id": role })),
		)
		.await;
		assert!(s.is_success());
	}
	assert_eq!(count(&db, "team_invites").await, 0);
	let update: serde_json::Value = sqlx::query_scalar(
		"SELECT diff FROM audit_events WHERE action = 'member.update' ORDER BY created_at DESC LIMIT 1",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert!(update.to_string().contains("invites_dropped"), "{update}");
}

#[db_test]
async fn invite_sign_up_ignores_case_and_never_makes_a_second_account(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, team) = user_with_team(&app, "alice").await;
	let (_, body) = invite(&app, &alice, team, "Dee@Example.com", MEMBER).await;
	assert_eq!(body["email"], "dee@example.com");
	let token = token_of(&body["link"]);
	// Someone signs up the same address in another case first (open registration).
	assert_eq!(
		call(
			&app,
			"POST",
			"/api/v1/auth/register",
			None,
			Some(
				json!({ "name": "Dee", "email": "DEE@example.com", "password": "correct horse battery" })
			),
		)
		.await
		.0,
		StatusCode::CREATED
	);
	let (s, _) = call(
		&app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(sign_up_body("dee@example.com", &token)),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT);
	let dees: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE lower(email) = 'dee@example.com'")
			.fetch_one(&db)
			.await
			.unwrap();
	assert_eq!(dees, 1);
}
