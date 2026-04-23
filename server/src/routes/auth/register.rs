use axum::{Json, extract::State, http::StatusCode};
use crypto::argon2;
use database::models::user::{NewUser, UserModel};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{AppState, error::ServerError};

#[derive(Deserialize)]
pub struct RegisterRequest {
	pub name: String,
	pub email: String,
	pub password: String,
}

pub async fn register(
	State(state): State<AppState>,
	Json(body): Json<RegisterRequest>,
) -> Result<StatusCode, ServerError> {
	let password_hash = argon2::hash(Zeroizing::new(body.password))
		.await
		.map_err(|_| ServerError::Internal)?;

	UserModel::create(
		state.pool,
		NewUser {
			name: body.name,
			email: body.email,
			pasword: password_hash,
		},
	)
	.await?;

	Ok(StatusCode::CREATED)
}
