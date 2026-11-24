//! Single sign-on, end to end against [`stub`](super::stub): the round trip through the real
//! routes (start, the provider, callback), ID-token validation, account creation and linking,
//! group mapping, and the rules every slice follows (other teams get 404, the secret is never
//! shown). Every test name holds `oidc`, so `-- oidc` runs them.

#![allow(
	clippy::many_single_char_names,
	clippy::too_many_lines,
	reason = "a table of scenarios reads best straight down"
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use serde_json::json;

use super::providers::{self, Input, Kind, Provider, Scope};
use super::stub::{CLIENT_ID, CLIENT_SECRET, Signer, Stub};
use crate::ids::{TeamId, UserId};
use crate::state::{AppState, Registration};
use crate::tests::{PgPool, count, db_test, scalar, state, user_with_team};
use crate::ui::testing::{Browser, get_html, page_main, post_signals, send, ui_session};

const PASSWORD: &str = "correct horse battery";

struct World {
	st: AppState,
	stub: Arc<Stub>,
	/// The oldest account: the instance administrator.
	admin: UserId,
	admin_cookie: String,
	admin_team: i64,
}

impl World {
	fn app_for(&self, registration: Registration) -> impl std::future::Future<Output = Router> {
		let mut st = self.st.clone();
		let mut config = (*st.config).clone();
		config.registration = registration;
		st.config = Arc::new(config);
		async move { crate::routes::routes(st).await.unwrap() }
	}
}

async fn world(db: PgPool) -> (World, Router) {
	let st = state(db.clone());
	let app = crate::routes::routes(st.clone()).await.unwrap();
	let (admin_cookie, admin_team) = ui_session(&app, "admin").await;
	let admin = UserId(crate::tests::user_id(&db, "admin").await);
	let stub = Stub::start().await;
	(
		World {
			st,
			stub,
			admin,
			admin_cookie,
			admin_team,
		},
		app,
	)
}

fn input(stub: &Stub, slug: &str) -> Input {
	Input {
		kind: Kind::Oidc,
		name: format!("Corp {slug}"),
		slug: slug.to_owned(),
		issuer: stub.url.clone(),
		client_id: CLIENT_ID.to_owned(),
		client_secret: CLIENT_SECRET.to_owned(),
		scopes: "openid email profile".to_owned(),
		jit: true,
		allow_unverified_email: false,
		domains: String::new(),
		group_claim: String::new(),
		group_roles: String::new(),
		enabled: true,
	}
}

async fn instance_provider(w: &World) -> Provider {
	providers::create(&w.st, w.admin, Scope::Instance, &input(&w.stub, "corp"))
		.await
		.unwrap()
}

/// Starts a sign-in with `id`, approves at the provider and returns the callback's answer.
trait Sso {
	async fn sso(
		&mut self,
		app: &Router,
		stub: &Stub,
		id: impl std::fmt::Display,
	) -> (StatusCode, HeaderMap, String);
}

impl Sso for Browser {
	async fn sso(
		&mut self,
		app: &Router,
		stub: &Stub,
		id: impl std::fmt::Display,
	) -> (StatusCode, HeaderMap, String) {
		let (status, headers, body) = self.get(app, &format!("/sso/{id}/start")).await;
		assert!(status.is_redirection(), "start: {status}\n{body}");
		let location = headers[header::LOCATION].to_str().unwrap().to_owned();
		let back = stub.approve(&location);
		self.get(app, &back).await
	}
}

async fn sessions_of(db: &PgPool, user: i64, method: &str) -> i64 {
	sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1 AND method = $2")
		.bind(user)
		.bind(method)
		.fetch_one(db)
		.await
		.unwrap()
}

async fn user_by_email(db: &PgPool, email: &str) -> Option<(i64, bool)> {
	sqlx::query_as("SELECT id, has_password FROM users WHERE email = $1")
		.bind(email)
		.fetch_optional(db)
		.await
		.unwrap()
}

async fn audit_actions(db: &PgPool) -> Vec<String> {
	sqlx::query_scalar("SELECT action FROM audit_events ORDER BY id")
		.fetch_all(db)
		.await
		.unwrap()
}

// --- the round trip --------------------------------------------------------------------

#[db_test]
async fn oidc_round_trip_creates_the_account_and_opens_a_session(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;

	let mut b = Browser::default();
	let (status, headers, body) = b.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(
		headers[header::REFRESH]
			.to_str()
			.unwrap()
			.ends_with("url=/"),
		"the page that continues to the app"
	);
	assert!(b.has_session());
	assert!(
		!b.jar.contains_key("pk_sso"),
		"the flow cookie is spent after the callback"
	);

	// PKCE and the client secret were checked by the provider, not just sent.
	assert_eq!(w.stub.token_requests.load(Ordering::SeqCst), 1);
	assert_eq!(w.stub.pkce_refused.load(Ordering::SeqCst), 0);
	assert_eq!(w.stub.client_refused.load(Ordering::SeqCst), 0);

	let (user, has_password) = user_by_email(&db, "ada@corp.test").await.unwrap();
	assert!(
		!has_password,
		"an account made by a provider has no password"
	);
	assert_eq!(sessions_of(&db, user, "sso").await, 1);
	assert_eq!(count(&db, "user_identities").await, 1);

	// A second sign-in is the same person, not a second account.
	let mut again = Browser::default();
	let (status, _, _) = again.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK);
	assert!(again.has_session());
	assert_eq!(
		scalar(
			&db,
			"SELECT COUNT(*) FROM users WHERE email = 'ada@corp.test'",
			None
		)
		.await,
		1
	);
	assert_eq!(sessions_of(&db, user, "sso").await, 2);

	let actions = audit_actions(&db).await;
	assert!(actions.iter().any(|a| a == "session.login"), "{actions:?}");
}

#[db_test]
async fn oidc_email_may_come_from_userinfo(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	w.stub.set(|b| b.email_at_userinfo = true);

	let mut b = Browser::default();
	let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(user_by_email(&db, "ada@corp.test").await.is_some());
}

#[test]
fn oidc_pkce_challenge_matches_the_rfc_7636_example() {
	// RFC 7636 appendix B.
	assert_eq!(
		super::oidc::challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
		"E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
	);
	let pair = super::oidc::pkce();
	assert!(
		pair.verifier.len() >= 43,
		"RFC 7636 §4.1: 43 to 128 characters"
	);
	assert_eq!(super::oidc::challenge_of(&pair.verifier), pair.challenge);
}

// --- ID token validation ---------------------------------------------------------------

type Tweak = fn(&mut super::stub::Behaviour);

#[db_test]
async fn oidc_refuses_every_bad_id_token(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	let users_before = count(&db, "users").await;

	let cases: Vec<(&str, Tweak)> = vec![
		("a signature by a key nobody published", |b| {
			b.signer = Signer::Unpublished;
		}),
		("an HS256 token signed with the client secret", |b| {
			b.signer = Signer::Shared;
		}),
		("another audience", |b| b.aud = Some(json!("someone-else"))),
		("several audiences and no azp", |b| {
			b.aud = Some(json!([CLIENT_ID, "other"]));
		}),
		("several audiences and another azp", |b| {
			b.aud = Some(json!([CLIENT_ID, "other"]));
			b.azp = Some("other".to_owned());
		}),
		("another issuer", |b| {
			b.iss = Some("http://evil.test".to_owned());
		}),
		("an expired token", |b| b.exp_in = -600),
		("a nonce of another sign-in", |b| {
			b.nonce = Some(Some("not-the-one".to_owned()));
		}),
		("no nonce", |b| b.nonce = Some(None)),
	];
	for (what, tweak) in cases {
		w.stub.set(|b| *b = super::stub::Behaviour::default());
		w.stub.set(tweak);
		let mut browser = Browser::default();
		let (status, _, body) = browser.sso(&app, &w.stub, p.id).await;
		assert!(status.is_client_error(), "{what}: {status}\n{body}");
		assert!(body.contains("could not be verified"), "{what}:\n{body}");
		assert!(!browser.has_session(), "{what}: no session");
		assert_eq!(
			count(&db, "users").await,
			users_before,
			"{what}: no account"
		);
	}
	assert_eq!(count(&db, "user_identities").await, 0);
	assert_eq!(
		scalar(
			&db,
			"SELECT COUNT(*) FROM sessions WHERE method = 'sso'",
			None
		)
		.await,
		0
	);

	// And the good ones still work: several audiences are fine when azp is us.
	w.stub.set(|b| *b = super::stub::Behaviour::default());
	w.stub.set(|b| {
		b.aud = Some(json!([CLIENT_ID, "other"]));
		b.azp = Some(CLIENT_ID.to_owned());
	});
	let mut ok = Browser::default();
	let (status, _, body) = ok.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(ok.has_session());
}

#[db_test]
async fn oidc_refuses_a_callback_the_browser_did_not_start(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;

	// Someone else's sign-in, finished in this browser (login CSRF): no matching cookie.
	let mut victim = Browser::default();
	let (_, headers, _) = victim.get(&app, &format!("/sso/{}/start", p.id)).await;
	let back = w.stub.approve(headers[header::LOCATION].to_str().unwrap());
	let mut attacker_browser = Browser::default();
	let (status, _, _) = attacker_browser.get(&app, &back).await;
	assert!(status.is_client_error());
	assert!(!attacker_browser.has_session());
	assert_eq!(
		w.stub.token_requests.load(Ordering::SeqCst),
		0,
		"nothing is exchanged for a callback with the wrong cookie"
	);

	// A callback with another state than the cookie's.
	let mut b = Browser::default();
	let (_, headers, _) = b.get(&app, &format!("/sso/{}/start", p.id)).await;
	let back = w.stub.approve(headers[header::LOCATION].to_str().unwrap());
	let forged = back.replace("state=", "state=x");
	let (status, _, _) = b.get(&app, &forged).await;
	assert!(status.is_client_error());
	assert!(!b.has_session());
	assert_eq!(w.stub.token_requests.load(Ordering::SeqCst), 0);

	// A callback that is replayed: the flow is spent.
	let mut r = Browser::default();
	let (_, headers, _) = r.get(&app, &format!("/sso/{}/start", p.id)).await;
	let back = w.stub.approve(headers[header::LOCATION].to_str().unwrap());
	let cookie_before = r.cookie();
	let (status, _, _) = r.get(&app, &back).await;
	assert_eq!(status, StatusCode::OK);
	let (status, _, _) = get_html(&app, &back, cookie_before.as_deref()).await;
	assert!(status.is_client_error(), "replay: {status}");
	assert_eq!(w.stub.token_requests.load(Ordering::SeqCst), 1);

	// The provider saying no ends the attempt politely.
	let mut n = Browser::default();
	n.get(&app, &format!("/sso/{}/start", p.id)).await;
	let (status, _, body) = n
		.get(&app, &format!("/sso/{}/callback?error=access_denied", p.id))
		.await;
	assert!(status.is_client_error());
	assert!(body.contains("did not sign you in"), "{body}");
}

#[db_test]
async fn oidc_a_disabled_or_unknown_provider_cannot_start(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let mut off = input(&w.stub, "off");
	off.enabled = false;
	let p = providers::create(&w.st, w.admin, Scope::Instance, &off)
		.await
		.unwrap();
	let mut b = Browser::default();
	let (status, _, _) = b.get(&app, &format!("/sso/{}/start", p.id)).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _, _) = b.get(&app, "/sso/999999/start").await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- accounts --------------------------------------------------------------------------

#[db_test]
async fn oidc_account_creation_respects_the_registration_policy(db: PgPool) {
	let (w, _open) = world(db.clone()).await;
	let p = instance_provider(&w).await;

	// Closed: the provider cannot be a way around it.
	let closed = w.app_for(Registration::Closed).await;
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&closed, &w.stub, p.id).await;
	assert!(status.is_client_error(), "{status}\n{body}");
	assert!(user_by_email(&db, "ada@corp.test").await.is_none());
	assert!(!b.has_session());

	// First user only: an instance that already has accounts is closed to newcomers.
	let first = w.app_for(Registration::FirstUser).await;
	let mut b = Browser::default();
	let (status, _, _) = b.sso(&first, &w.stub, p.id).await;
	assert!(status.is_client_error());
	assert!(user_by_email(&db, "ada@corp.test").await.is_none());

	// A provider that does not create accounts, on an open instance.
	let mut no_jit = input(&w.stub, "nojit");
	no_jit.jit = false;
	let q = providers::create(&w.st, w.admin, Scope::Instance, &no_jit)
		.await
		.unwrap();
	let open = w.app_for(Registration::Open).await;
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&open, &w.stub, q.id).await;
	assert!(status.is_client_error());
	assert!(body.contains("does not create accounts"), "{body}");
	assert!(user_by_email(&db, "ada@corp.test").await.is_none());

	// Open: created.
	let mut b = Browser::default();
	let (status, _, _) = b.sso(&open, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK);
	assert!(user_by_email(&db, "ada@corp.test").await.is_some());
}

#[db_test]
async fn oidc_an_unverified_email_is_not_trusted(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	for verified in [Some(false), None] {
		w.stub.set(|b| b.email_verified = verified);
		let mut b = Browser::default();
		let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
		assert!(status.is_client_error(), "{verified:?}: {status}");
		assert!(body.contains("verified email"), "{body}");
		assert!(user_by_email(&db, "ada@corp.test").await.is_none());
	}

	// An administrator can say a provider's emails are good anyway.
	let mut lax = input(&w.stub, "lax");
	lax.allow_unverified_email = true;
	let q = providers::create(&w.st, w.admin, Scope::Instance, &lax)
		.await
		.unwrap();
	let mut b = Browser::default();
	let (status, _, _) = b.sso(&app, &w.stub, q.id).await;
	assert_eq!(status, StatusCode::OK);
	assert!(user_by_email(&db, "ada@corp.test").await.is_some());
}

#[db_test]
async fn oidc_an_existing_email_is_linked_only_after_the_password(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	// `ada` already has an account here, made with a password.
	let (_token, _team) = user_with_team(&app, "ada").await;
	let (ada, _) = user_by_email(&db, "ada@example.com").await.unwrap();
	w.stub.set(|b| b.email = Some("ada@example.com".to_owned()));

	let mut b = Browser::default();
	let (status, headers, _) = b.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK);
	assert!(
		headers[header::REFRESH]
			.to_str()
			.unwrap()
			.ends_with("url=/sso/link"),
		"asked for the password first"
	);
	assert!(
		!b.has_session(),
		"the provider alone does not sign anyone in"
	);
	assert_eq!(count(&db, "user_identities").await, 0);
	assert_eq!(sessions_of(&db, ada, "sso").await, 0);

	let (status, _, page) = b.get(&app, "/sso/link").await;
	assert_eq!(status, StatusCode::OK);
	assert!(page.contains("ada@example.com"));

	// Wrong passwords link nothing.
	b.post(&app, "/sso/link", &json!({ "password": "wrong one" }))
		.await;
	assert!(!b.has_session());
	assert_eq!(count(&db, "user_identities").await, 0);

	// The right one links and signs in.
	b.post(&app, "/sso/link", &json!({ "password": PASSWORD }))
		.await;
	assert!(b.has_session());
	assert_eq!(count(&db, "user_identities").await, 1);
	assert!(
		audit_actions(&db)
			.await
			.iter()
			.any(|a| a == "identity.link")
	);

	// From now on the provider signs ada in directly.
	let mut next = Browser::default();
	let (status, headers, _) = next.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK);
	assert!(
		headers[header::REFRESH]
			.to_str()
			.unwrap()
			.ends_with("url=/")
	);
	assert!(next.has_session());
}

#[db_test]
async fn oidc_five_wrong_passwords_end_the_link_attempt(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	user_with_team(&app, "ada").await;
	w.stub.set(|b| b.email = Some("ada@example.com".to_owned()));

	let mut b = Browser::default();
	b.sso(&app, &w.stub, p.id).await;
	for _ in 0..crate::auth::flow::MAX_ATTEMPTS {
		b.post(&app, "/sso/link", &json!({ "password": "nope" }))
			.await;
	}
	// Too late for the right one.
	b.post(&app, "/sso/link", &json!({ "password": PASSWORD }))
		.await;
	assert!(!b.has_session());
	assert_eq!(count(&db, "user_identities").await, 0);
}

#[db_test]
async fn oidc_an_account_made_by_a_provider_cannot_be_taken_over_by_another(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	let mut first = Browser::default();
	first.sso(&app, &w.stub, p.id).await;
	assert!(first.has_session());

	// Another provider claims the same email with another subject: the account has no
	// password to prove it with, so it is not linked.
	let other = providers::create(&w.st, w.admin, Scope::Instance, &input(&w.stub, "second"))
		.await
		.unwrap();
	w.stub.set(|b| b.subject = "sub-2".to_owned());
	let mut b = Browser::default();
	let (_, _, _) = b.sso(&app, &w.stub, other.id).await;
	b.post(&app, "/sso/link", &json!({ "password": "anything at all" }))
		.await;
	assert!(!b.has_session());
	assert_eq!(count(&db, "user_identities").await, 1);
}

// --- teams -----------------------------------------------------------------------------

async fn team_with_provider(w: &World, groups: &str) -> (i64, Provider) {
	let team = w.admin_team;
	let mut i = input(&w.stub, "acme");
	i.group_claim = "groups".to_owned();
	i.group_roles = groups.to_owned();
	// The instance administrator lists it, so the domain is approved at once.
	i.domains = "corp.test".to_owned();
	let p = providers::create(&w.st, w.admin, Scope::Team(TeamId(team)), &i)
		.await
		.unwrap();
	(team, p)
}

async fn role_of(db: &PgPool, team: i64, email: &str) -> Option<String> {
	sqlx::query_scalar(
		"SELECT r.name FROM team_members m JOIN users u ON u.id = m.user_id
		 JOIN roles r ON r.id = m.role_id WHERE m.team_id = $1 AND u.email = $2",
	)
	.bind(team)
	.bind(email)
	.fetch_optional(db)
	.await
	.unwrap()
}

#[db_test]
async fn oidc_groups_map_to_team_roles_and_never_demote_the_last_owner(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let (team, p) = team_with_provider(&w, "admins = Owner\nstaff = Member").await;

	// A newcomer in `staff` joins as a Member.
	w.stub.set(|b| {
		b.subject = "s-1".to_owned();
		b.email = Some("sam@corp.test".to_owned());
		b.groups = Some(json!(["staff"]));
	});
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(
		role_of(&db, team, "sam@corp.test").await.as_deref(),
		Some("Member")
	);

	// The next sign-in carries a new group: the role follows.
	w.stub.set(|b| b.groups = Some(json!(["admins", "staff"])));
	let mut again = Browser::default();
	again.sso(&app, &w.stub, p.id).await;
	assert_eq!(
		role_of(&db, team, "sam@corp.test").await.as_deref(),
		Some("Owner")
	);

	// Someone in no mapped group gets an account but no seat.
	w.stub.set(|b| {
		b.subject = "s-2".to_owned();
		b.email = Some("nina@corp.test".to_owned());
		b.groups = Some(json!(["interns"]));
	});
	let mut c = Browser::default();
	c.sso(&app, &w.stub, p.id).await;
	assert!(user_by_email(&db, "nina@corp.test").await.is_some());
	assert_eq!(role_of(&db, team, "nina@corp.test").await, None);

	// The team's only Owner linking through a provider that says "staff" stays Owner.
	sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND role_id = 1 AND user_id <> $2")
		.bind(team)
		.bind(w.admin.0)
		.execute(&db)
		.await
		.unwrap();
	w.stub.set(|b| {
		b.subject = "s-admin".to_owned();
		b.email = Some("admin@example.com".to_owned());
		b.groups = Some(json!(["staff"]));
	});
	let mut owner = Browser::default();
	owner.sso(&app, &w.stub, p.id).await;
	owner
		.post(&app, "/sso/link", &json!({ "password": PASSWORD }))
		.await;
	assert!(owner.has_session());
	assert_eq!(
		role_of(&db, team, "admin@example.com").await.as_deref(),
		Some("Owner"),
		"the last Owner is not demoted by a group"
	);
}

#[db_test]
async fn oidc_a_provider_of_another_team_is_a_404(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let (team, p) = team_with_provider(&w, "").await;
	let (eve_cookie, eve_team) = ui_session(&app, "eve").await;

	// Eve is no member of the admin's team: her pages for it do not exist.
	for uri in [
		format!("/teams/{team}/settings/authentication"),
		format!(
			"/teams/{team}/settings/authentication/team/providers/{}",
			p.id
		),
	] {
		let (status, _, _) = get_html(&app, &uri, Some(&eve_cookie)).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
	}
	// Through her own team's URL the provider is not hers either.
	let (status, _, _) = get_html(
		&app,
		&format!(
			"/teams/{eve_team}/settings/authentication/team/providers/{}",
			p.id
		),
		Some(&eve_cookie),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _, _) = post_signals(
		&app,
		&format!(
			"/teams/{eve_team}/settings/authentication/team/providers/{}/delete",
			p.id
		),
		Some(&eve_cookie),
		&json!({}),
	)
	.await;
	assert!(
		!status.is_success() || status == StatusCode::OK,
		"an SSE answer carries the refusal in its body"
	);
	assert_eq!(count(&db, "auth_providers").await, 1, "nothing was deleted");

	// The service says the same.
	let eve = UserId(crate::tests::user_id(&db, "eve").await);
	let listed = providers::list(&db, eve, Scope::Team(TeamId(team))).await;
	assert!(listed.is_err());
	let found = providers::find(&db, eve, Scope::Team(TeamId(eve_team)), p.id).await;
	assert!(matches!(found, Err(crate::error::ServerError::NotFound)));

	// A team's provider shows only on that team's sign-in page.
	let (_, _, plain) = get_html(&app, "/login", None).await;
	assert!(!plain.contains(&p.name), "not on the general page");
	let (_, _, scoped) = get_html(&app, "/login?team=admin", None).await;
	assert!(scoped.contains(&format!("Sign in with {}", p.name)));
	let (_, _, other) = get_html(&app, "/login?team=eve", None).await;
	assert!(!other.contains(&p.name));
}

#[db_test]
async fn oidc_instance_providers_belong_to_the_administrator(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let (eve_cookie, eve_team) = ui_session(&app, "eve").await;
	let eve = UserId(crate::tests::user_id(&db, "eve").await);

	// Eve owns her team, but the instance's providers are not hers.
	let made = providers::create(&w.st, eve, Scope::Instance, &input(&w.stub, "sneaky")).await;
	assert!(matches!(made, Err(crate::error::ServerError::Forbidden)));
	assert_eq!(count(&db, "auth_providers").await, 0);

	let p = instance_provider(&w).await;
	assert!(providers::list(&db, eve, Scope::Instance).await.is_err());
	assert!(
		providers::find(&db, eve, Scope::Instance, p.id)
			.await
			.is_err()
	);
	let (_, _, page) = get_html(
		&app,
		&format!("/teams/{eve_team}/settings/authentication"),
		Some(&eve_cookie),
	)
	.await;
	assert!(!page_main(&page).contains(&p.name), "not even listed");
	let (_, _, mine) = get_html(
		&app,
		&format!("/teams/{}/settings/authentication", w.admin_team),
		Some(&w.admin_cookie),
	)
	.await;
	assert!(page_main(&mine).contains(&p.name));

	// The instance's provider is on everyone's sign-in page.
	let (_, _, login) = get_html(&app, "/login", None).await;
	assert!(login.contains(&format!("Sign in with {}", p.name)));
	assert!(login.contains(&format!("/sso/{}/start", p.id)));
}

// --- secrets, forged requests, escaping ------------------------------------------------

#[db_test]
async fn oidc_the_client_secret_is_write_only(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	let mut b = Browser::default();
	b.sso(&app, &w.stub, p.id).await;

	let base = format!("/teams/{}/settings/authentication", w.admin_team);
	for uri in [base.clone(), format!("{base}/instance/providers/{}", p.id)] {
		let (status, _, page) = get_html(&app, &uri, Some(&w.admin_cookie)).await;
		assert_eq!(status, StatusCode::OK, "{uri}");
		assert!(!page.contains(CLIENT_SECRET), "{uri} shows the secret");
	}

	// Stored sealed, and kept out of the audit log.
	let stored: Vec<u8> = sqlx::query_scalar("SELECT client_secret FROM auth_providers")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&stored).contains(CLIENT_SECRET));
	let diffs: Vec<String> = sqlx::query_scalar("SELECT summary || diff::text FROM audit_events")
		.fetch_all(&db)
		.await
		.unwrap();
	assert!(
		!diffs.iter().any(|d| d.contains(CLIENT_SECRET)),
		"{diffs:?}"
	);
	assert!(
		audit_actions(&db)
			.await
			.iter()
			.any(|a| a == "auth_provider.create")
	);

	// An edit that leaves the field empty keeps the secret, and the sign-in still works.
	let mut edit = input(&w.stub, "corp");
	edit.client_secret = String::new();
	edit.name = "Renamed".to_owned();
	providers::update(&w.st, w.admin, Scope::Instance, p.id, &edit)
		.await
		.unwrap();
	let mut again = Browser::default();
	let (status, _, body) = again.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(w.stub.client_refused.load(Ordering::SeqCst), 0);
}

#[db_test]
async fn oidc_forged_posts_are_refused(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let uri = format!(
		"/teams/{}/settings/authentication/instance/providers",
		w.admin_team
	);
	let signals = json!({
		"p_kind": "oidc", "p_name": "Evil", "p_slug": "evil", "p_issuer": w.stub.url,
		"p_client_id": CLIENT_ID, "p_secret": "s", "p_scopes": "openid", "p_jit": true,
		"p_unverified": false, "p_claim": "", "p_groups": "", "p_enabled": true,
	});
	// From another origin: refused before anything happens.
	for origin in [
		Some("http://evil.test"),
		Some("http://podkit.test.evil.test"),
	] {
		let (status, _, _) = send(
			&app,
			"POST",
			&uri,
			Some(&w.admin_cookie),
			Some(&signals.to_string()),
			origin,
		)
		.await;
		assert_eq!(status, StatusCode::FORBIDDEN, "{origin:?}");
	}
	// Nobody signed in at all.
	let _ = post_signals(&app, &uri, None, &signals).await;
	assert_eq!(count(&db, "auth_providers").await, 0);

	// The same request from the right origin works, so the refusals above are the guard's.
	let (status, _, body) = post_signals(&app, &uri, Some(&w.admin_cookie), &signals).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(count(&db, "auth_providers").await, 1);
}

#[db_test]
async fn oidc_provider_names_are_escaped_on_the_sign_in_page(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let mut i = input(&w.stub, "xss");
	i.name = "<script>alert(1)</script>".to_owned();
	providers::create(&w.st, w.admin, Scope::Instance, &i)
		.await
		.unwrap();
	let (_, _, login) = get_html(&app, "/login", None).await;
	assert!(!login.contains("<script>alert(1)</script>"));
	assert!(login.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[test]
fn oidc_the_ui_holds_no_queries_and_no_authorization() {
	// The invariant of the UI slice, kept here for the pages this card adds.
	let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
	let mut bad = Vec::new();
	let mut stack = vec![root];
	while let Some(dir) = stack.pop() {
		for entry in std::fs::read_dir(dir).unwrap() {
			let path = entry.unwrap().path();
			if path.is_dir() {
				stack.push(path);
			} else if path.extension().is_some_and(|e| e == "rs")
				&& !path.ends_with("testing.rs")
				&& !path.to_string_lossy().contains("tests")
			{
				let text = std::fs::read_to_string(&path).unwrap();
				for (n, line) in text.lines().enumerate() {
					let code = line.split("//").next().unwrap_or("");
					if code.contains("sqlx::")
						|| code.contains("query!")
						|| code.contains("query_as")
						|| code.contains("query_scalar")
						|| code.contains("authorize(")
					{
						bad.push(format!("{}:{}", path.display(), n + 1));
					}
				}
			}
		}
	}
	assert!(bad.is_empty(), "{bad:?}");
}

#[db_test]
async fn throttle_link_password_guesses_are_counted_across_new_link_flows(db: PgPool) {
	let (w, app) = world(db.clone()).await;
	let p = instance_provider(&w).await;
	user_with_team(&app, "ada").await;
	w.stub.set(|b| b.email = Some("ada@example.com".to_owned()));

	// The flow's own five tries end one link attempt; a provider that asserts the victim's
	// email can start another at will. The account's count carries over.
	let mut last = String::new();
	for _ in 0..crate::throttle::ACCOUNT_LIMIT {
		let mut b = Browser::default();
		b.sso(&app, &w.stub, p.id).await;
		let (_, _, body) = b
			.post(&app, "/sso/link", &json!({ "password": "nope" }))
			.await;
		last = body;
	}
	assert!(
		!last.contains("Too many attempts"),
		"the fifth is still a plain miss"
	);
	crate::throttle::tests::hold(&db).await;
	let mut b = Browser::default();
	b.sso(&app, &w.stub, p.id).await;
	let (_, _, body) = b
		.post(&app, "/sso/link", &json!({ "password": PASSWORD }))
		.await;
	assert!(body.contains("Too many attempts, try again in "), "{body}");
	assert!(!b.has_session());
	assert_eq!(count(&db, "user_identities").await, 0);
}

/// SEC-07: a team administrator's own identity provider cannot create accounts for email it has
/// not been given: only for domains the instance administrator approved.
#[db_test]
async fn sec_cleanup_oidc_a_team_provider_cannot_claim_a_domain_nobody_approved(db: PgPool) {
	use super::domains;

	let (w, app) = world(db.clone()).await;
	let (eve_cookie, eve_team) = ui_session(&app, "eve").await;
	let eve = UserId(crate::tests::user_id(&db, "eve").await);

	// Eve runs the identity provider of her own team and lists the domain she wants.
	let mut i = input(&w.stub, "eves");
	i.domains = "corp.test".to_owned();
	i.allow_unverified_email = true;
	let p = providers::create(&w.st, eve, Scope::Team(TeamId(eve_team)), &i)
		.await
		.unwrap();
	assert!(
		!p.allow_unverified_email,
		"a team provider cannot vouch for an unverified email"
	);
	let listed = domains::list(&db, p.id).await.unwrap();
	assert_eq!(
		listed,
		vec![domains::Domain {
			name: "corp.test".to_owned(),
			approved: false
		}]
	);

	// A person nobody has registered cannot be signed up by it, even in its own domain.
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
	assert!(status.is_client_error(), "{status}\n{body}");
	assert!(body.contains("not approved"), "{body}");
	assert!(user_by_email(&db, "ada@corp.test").await.is_none());
	assert!(!b.has_session());

	// Nor does an unverified address get in, whatever the form said.
	w.stub.set(|b| b.email_verified = Some(false));
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
	assert!(status.is_client_error(), "{status}");
	assert!(body.contains("verified email"), "{body}");
	w.stub.set(|b| b.email_verified = Some(true));

	// She cannot approve her own domain; the instance administrator sees it waiting.
	assert!(matches!(
		domains::approve(&w.st, eve, p.id, "corp.test").await,
		Err(crate::error::ServerError::Forbidden)
	));
	assert!(domains::pending(&db, eve).await.is_err());
	let waiting = domains::pending(&db, w.admin).await.unwrap();
	assert_eq!(
		waiting
			.iter()
			.map(|d| (d.provider.as_str(), d.domain.as_str()))
			.collect::<Vec<_>>(),
		vec![("Corp eves", "corp.test")]
	);
	let (status, _, page) = get_html(
		&app,
		&format!("/teams/{}/settings/authentication", w.admin_team),
		Some(&w.admin_cookie),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert!(page.contains("Domains waiting for approval"), "{page}");
	let (status, _, eve_page) = get_html(
		&app,
		&format!("/teams/{eve_team}/settings/authentication"),
		Some(&eve_cookie),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert!(!eve_page.contains("Domains waiting for approval"));

	// Another domain stays closed even after the first is approved.
	domains::approve(&w.st, w.admin, p.id, "corp.test")
		.await
		.unwrap();
	assert!(domains::pending(&db, w.admin).await.unwrap().is_empty());
	w.stub
		.set(|b| b.email = Some("mallory@evil.test".to_owned()));
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
	assert!(status.is_client_error(), "{status}");
	assert!(body.contains("not approved"), "{body}");
	assert!(user_by_email(&db, "mallory@evil.test").await.is_none());

	// The approved one works; the approval is in the audit log.
	w.stub.set(|b| b.email = Some("ada@corp.test".to_owned()));
	let mut b = Browser::default();
	let (status, _, body) = b.sso(&app, &w.stub, p.id).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(user_by_email(&db, "ada@corp.test").await.is_some());
	assert!(
		audit_actions(&db)
			.await
			.iter()
			.any(|a| a == "auth_provider.domain_approve")
	);

	// Dropping the domain from the list closes it again.
	let mut i = input(&w.stub, "eves");
	i.domains = String::new();
	providers::update(&w.st, eve, Scope::Team(TeamId(eve_team)), p.id, &i)
		.await
		.unwrap();
	assert!(domains::list(&db, p.id).await.unwrap().is_empty());
}

#[test]
fn sec_cleanup_oidc_domains_are_parsed_strictly() {
	use super::domains::{of_email, parse};
	assert_eq!(
		parse("Example.com, @corp.example\nexample.com").unwrap(),
		vec!["example.com", "corp.example"]
	);
	assert!(parse("").unwrap().is_empty());
	assert!(parse("localhost").is_err());
	assert!(parse("a..com").is_err());
	assert!(parse("*.com").is_err());
	assert_eq!(of_email("Ada@Corp.Test").as_deref(), Some("corp.test"));
	assert_eq!(of_email("nobody"), None);
}
