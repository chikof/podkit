use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies};
use zeroize::Zeroizing;

use database::models::user::UserModel;

use crate::{error::ServerError, AppState};

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
	let user = UserModel::authenticate(state.pool, &body.email, Zeroizing::new(body.password))
		.await
		.map_err(|_| ServerError::Internal)?
		.ok_or(ServerError::InvalidCredentials)?;

	let token = state
		.tokens
		.issue(user.id)
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
