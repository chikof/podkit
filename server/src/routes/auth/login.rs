use axum::{Json, extract::State};
use crypto::argon2;
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies};
use zeroize::Zeroizing;

use crate::{AppState, error::ServerError};

#[derive(Deserialize)]
pub struct LoginRequest {
	pub email: String,
	pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
	pub token: String,
}

pub async fn login(
	State(state): State<AppState>,
	cookies: Cookies,
	Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ServerError> {
	let Some(user) = state.users.find_by_email(&body.email).await? else {
		// Run verify anyway to prevent timing-based user enumeration
		argon2::verify(Zeroizing::new(body.password), argon2::DUMMY_HASH.clone())
			.await
			.ok();
		return Err(ServerError::InvalidCredentials);
	};

	let valid = state
		.password_hasher
		.verify(&body.password, &user.password_hash)
		.await?;

	if !valid {
		return Err(ServerError::InvalidCredentials);
	}

	let token = state
		.tokens
		.issue(user.id.0)
		.map_err(|_| ServerError::Internal)?;

	cookies.add(
		Cookie::build(("session", token.clone()))
			.http_only(true)
			.secure(true)
			.same_site(tower_cookies::cookie::SameSite::Strict)
			.max_age(time::Duration::hours(24))
			.path("/")
			.build(),
	);

	Ok(Json(LoginResponse { token }))
}
