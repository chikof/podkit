use axum::{
	Json, Router,
	extract::State,
	http::{HeaderMap, StatusCode},
	routing::post,
};
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies, cookie::SameSite};
use zeroize::Zeroizing;

use super::{AuthUser, SessionToken, service, session, users};
use crate::{error::ServerError, state::AppState};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/register", post(register))
		.route("/login", post(login))
		.route("/logout", post(logout))
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
}

async fn register(
	State(s): State<AppState>,
	Json(body): Json<RegisterRequest>,
) -> Result<StatusCode, ServerError> {
	service::register(
		&s.db,
		s.config.registration,
		&body.name,
		&body.email,
		&body.password,
	)
	.await?;

	Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
pub struct Credentials {
	pub email: String,
	pub password: String,
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
fn reached_over_https(headers: &HeaderMap) -> bool {
	headers
		.get("x-forwarded-proto")
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.split(',').next())
		.is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

/// Verifies credentials, opens a session and sets the `session` cookie.
/// Returns the session token. The cookie is `Secure` only for HTTPS requests,
/// so sign-in also works on plain-HTTP homelab installs.
pub async fn log_in(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	body: Credentials,
) -> Result<String, ServerError> {
	let Some(user) = users::find_by_email(&s.db, &body.email).await? else {
		// Run verify anyway to prevent timing-based user enumeration
		crypto::verify(Zeroizing::new(body.password), crypto::DUMMY_HASH.clone())
			.await
			.ok();
		return Err(ServerError::InvalidCredentials);
	};

	let valid = crypto::verify(Zeroizing::new(body.password), user.password_hash)
		.await
		.map_err(|e| hashing_failed(&e))?;
	if !valid {
		return Err(ServerError::InvalidCredentials);
	}

	let token = session::create(&s.db, user.id).await?;

	cookies.add(
		Cookie::build(("session", token.clone()))
			.http_only(true)
			.secure(reached_over_https(headers))
			.same_site(SameSite::Strict)
			.max_age(session::SESSION_TTL)
			.path("/")
			.build(),
	);

	Ok(token)
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
