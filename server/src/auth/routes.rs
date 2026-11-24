use axum::{
	Json, Router,
	extract::State,
	http::{HeaderMap, StatusCode},
	routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies, cookie::SameSite};
use zeroize::Zeroizing;

use super::{AuthUser, SessionToken, service, session, users};
use crate::audit::{Diff, Target};
use crate::throttle::{self, Who};
use crate::{error::ServerError, state::AppState};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/register", post(register))
		.route("/login", post(login))
		.route("/logout", post(logout))
		.route("/invites/accept", post(accept_invite))
}

/// `GET /me`: who the caller is. Outside `/auth` because tokens may not reach that.
pub fn me_router() -> Router<AppState> {
	Router::new().route("/me", get(me))
}

/// The caller, and for an API token what it may do. Never carries the secret.
#[derive(Serialize)]
struct MeResponse {
	id: crate::ids::UserId,
	name: String,
	email: String,
	/// `None` for a session.
	token: Option<MeToken>,
}

#[derive(Serialize)]
struct MeToken {
	id: crate::ids::ApiTokenId,
	team_id: crate::ids::TeamId,
	scopes: Vec<&'static str>,
	/// Empty means every project of the team.
	project_ids: Vec<crate::ids::ProjectId>,
}

/// Says who the caller is: the account, and the scopes and limits of the token used, so a
/// command line can show them and tell what it may do before it tries. Any signed-in
/// caller may ask about themselves; a token needs only `read`.
async fn me(State(s): State<AppState>, user: AuthUser) -> Result<Json<MeResponse>, ServerError> {
	let name = users::name_of(&s.db, user.id).await?;
	let email = users::email_of(&s.db, user.id).await?;
	let (Some(name), Some(email)) = (name, email) else {
		return Err(ServerError::InvalidToken);
	};
	Ok(Json(MeResponse {
		id: user.id,
		name,
		email,
		token: user.grant.map(|g| MeToken {
			id: g.token,
			team_id: g.team,
			scopes: g.scopes.iter().map(|s| s.as_str()).collect(),
			project_ids: g.projects,
		}),
	}))
}

fn hashing_failed(e: &anyhow::Error) -> ServerError {
	tracing::error!(error = %format!("{e:#}"), "password hashing failed");
	ServerError::Internal
}

#[derive(Deserialize)]
struct RegisterRequest {
	name: String,
	email: String,
	password: String,
	/// The first-run setup token from podkit's log; needed only while no account exists.
	#[serde(default)]
	setup_token: Option<String>,
	/// The token of an invite link: signs up the invited address whatever the registration
	/// policy says, and joins its team.
	#[serde(default)]
	invite: Option<String>,
}

async fn register(
	State(s): State<AppState>,
	Json(body): Json<RegisterRequest>,
) -> Result<StatusCode, ServerError> {
	if let Some(invite) = body.invite.as_deref() {
		crate::members::invites::sign_up(
			&s.db,
			invite,
			&body.name,
			Some(&body.email),
			&body.password,
		)
		.await?;
		return Ok(StatusCode::CREATED);
	}
	service::register(
		&s.db,
		s.config.registration,
		&s.setup,
		service::SignUp {
			name: &body.name,
			email: &body.email,
			password: &body.password,
			setup_token: body.setup_token.as_deref(),
		},
	)
	.await?;

	Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
struct AcceptInviteRequest {
	token: String,
}

#[derive(Serialize)]
struct AcceptedInvite {
	team_id: crate::ids::TeamId,
}

/// Joins the team of an invite for the signed-in person's own address. A session only: the
/// token gate keeps API tokens out of `/auth`.
async fn accept_invite(
	State(s): State<AppState>,
	user: AuthUser,
	Json(body): Json<AcceptInviteRequest>,
) -> Result<Json<AcceptedInvite>, ServerError> {
	let team_id = crate::members::invites::accept(&s.db, user.id, &body.token).await?;
	Ok(Json(AcceptedInvite { team_id }))
}

#[derive(Deserialize)]
pub struct Credentials {
	pub email: String,
	pub password: String,
	/// A code from the authenticator app, or a recovery code, for an account with two-step
	/// verification. Without one such an account is told to send it.
	#[serde(default)]
	pub code: Option<String>,
}

#[derive(Serialize)]
struct LoginResponse {
	token: String,
}

async fn login(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Json(body): Json<Credentials>,
) -> Result<Json<LoginResponse>, ServerError> {
	let token = log_in(&s, &cookies, &headers, body).await?;
	Ok(Json(LoginResponse { token }))
}

/// podkit serves plain HTTP; HTTPS ends at a reverse proxy, which reports the
/// original scheme in `X-Forwarded-Proto`. A client forging the header only
/// makes its own cookie stricter.
pub fn reached_over_https(headers: &HeaderMap) -> bool {
	headers
		.get("x-forwarded-proto")
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.split(',').next())
		.is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

/// Checks an email and password and says whose they are; no session is opened.
pub async fn verify_password(
	db: &sqlx::PgPool,
	email: &str,
	password: String,
) -> Result<crate::ids::UserId, ServerError> {
	let found = users::find_by_email(db, email).await?;
	// An email nobody has is counted and locked like one somebody has, so neither the
	// answer nor the timing says which exist (SEC-01).
	let attempt = throttle::begin(db, Who::of_email(found.as_ref().map(|u| u.id), email)).await?;
	let Some(user) = found else {
		// Run verify anyway to prevent timing-based user enumeration
		crypto::verify(Zeroizing::new(password), crypto::DUMMY_HASH.clone())
			.await
			.ok();
		attempt.fail().await;
		return Err(ServerError::InvalidCredentials);
	};

	// An account made by an identity provider has a random password nobody knows: this
	// cannot succeed for it, and says the same as a wrong one.
	let valid = crypto::verify(Zeroizing::new(password), user.password_hash)
		.await
		.map_err(|e| hashing_failed(&e))?;
	attempt.finish(valid).await;
	if !valid {
		return Err(ServerError::InvalidCredentials);
	}
	Ok(user.id)
}

/// The `session` cookie. `Secure` only for HTTPS requests, so sign-in also works on
/// plain-HTTP homelab installs.
pub fn session_cookie(headers: &HeaderMap, token: &str) -> Cookie<'static> {
	Cookie::build(("session", token.to_owned()))
		.http_only(true)
		.secure(reached_over_https(headers))
		.same_site(SameSite::Strict)
		.max_age(session::SESSION_TTL)
		.path("/")
		.build()
}

/// Opens a session for `user`, sets the cookie and records the sign-in in the audit log
/// of the person's teams. Returns the session token. Whatever proves who the person is
/// (password and second factor, an identity provider, a passkey) has happened before.
pub async fn open_session(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	user: crate::ids::UserId,
	method: session::Method,
) -> Result<String, ServerError> {
	let token = session::create_with(&s.db, user, method).await?;
	cookies.add(session_cookie(headers, &token));
	// A full sign-in: the account's wrong guesses so far no longer count.
	throttle::clear_account(&s.db, user).await;
	super::record_everywhere(
		&s.db,
		user,
		"session.login",
		Target::none("session").named(format!("({})", method.as_str())),
		Diff::new().set("method", method.as_str()),
	)
	.await;
	Ok(token)
}

/// Verifies credentials, opens a session and sets the `session` cookie.
/// Returns the session token. An account with two-step verification must send `code`
/// (the JSON API has no second page to ask on); a person who has only a passkey signs in
/// on the web or with an API token.
pub async fn log_in(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	body: Credentials,
) -> Result<String, ServerError> {
	let code = body.code.clone();
	let user = verify_password(&s.db, &body.email, body.password).await?;
	if crate::two_factor::service::enrolled(&s.db, user).await? {
		let Some(code) = code.filter(|c| !c.trim().is_empty()) else {
			return Err(ServerError::TwoFactorCodeRequired);
		};
		let attempt = throttle::begin(&s.db, Who::User(user)).await?;
		let right = crate::two_factor::service::check_code(s, user, &code).await?;
		attempt.finish(right).await;
		if !right {
			super::record_everywhere(
				&s.db,
				user,
				"session.login_failed",
				Target::none("session").named("(second factor)"),
				Diff::new().set("method", "password"),
			)
			.await;
			return Err(ServerError::InvalidCredentials);
		}
	}
	open_session(s, cookies, headers, user, session::Method::Password).await
}

async fn logout(
	State(s): State<AppState>,
	cookies: Cookies,
	_user: AuthUser,
	SessionToken(token): SessionToken,
) -> Result<StatusCode, ServerError> {
	log_out(&s, &cookies, &token).await?;
	Ok(StatusCode::NO_CONTENT)
}

/// Revokes the session and clears the `session` cookie.
pub async fn log_out(s: &AppState, cookies: &Cookies, token: &str) -> Result<(), ServerError> {
	session::revoke(&s.db, token).await?;

	// Removal only matches the cookie when the path is the one login set.
	cookies.remove(Cookie::build(("session", "")).path("/").build());
	Ok(())
}
