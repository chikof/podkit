use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies, cookie::SameSite};
use zeroize::Zeroizing;

use super::{AuthUser, SessionToken, session, users, validate_email};
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
	let email = validate_email(&body.email)
		.map_err(|e| ServerError::Validation(e.to_string()))?
		.normalize();

	let password_hash = crypto::hash(Zeroizing::new(body.password))
		.await
		.map_err(|e| hashing_failed(&e))?;

	users::insert(&s.db, &body.name, &email, &password_hash)
		.await
		.map_err(|e| {
			if crate::db::is_unique_violation(&e, users::EMAIL_UNIQUE) {
				ServerError::EmailTaken
			} else {
				e.into()
			}
		})?;

	Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
pub struct Credentials {
	email: String,
	password: String,
}

#[derive(Serialize)]
struct LoginResponse {
	token: String,
}

async fn login(
	State(s): State<AppState>,
	cookies: Cookies,
	Json(body): Json<Credentials>,
) -> Result<Json<LoginResponse>, ServerError> {
	let token = log_in(&s, &cookies, body).await?;
	Ok(Json(LoginResponse { token }))
}

/// Verifies credentials, opens a session and sets the `session` cookie.
/// Returns the session token.
pub async fn log_in(
	s: &AppState,
	cookies: &Cookies,
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
			.secure(true)
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
