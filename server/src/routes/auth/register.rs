use axum::{Json, extract::State, http::StatusCode};
use crypto::generate_id;
use podkit_core::domain::shared::ids::UserId;
use podkit_core::domain::user::User;
use podkit_core::domain::user::value_objects::Email;
use serde::Deserialize;

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
	let email = Email::new(&body.email).map_err(|e| ServerError::Validation(e.to_string()))?;

	if state.users.exists_by_email(email.as_str()).await? {
		return Err(ServerError::EmailTaken);
	}

	let password_hash = state.password_hasher.hash(&body.password).await?;

	let user = User::new(UserId(generate_id()), email, password_hash, body.name);
	state.users.save(&user).await?;

	Ok(StatusCode::CREATED)
}
