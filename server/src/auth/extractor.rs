use axum::{extract::FromRequestParts, http::request::Parts, RequestPartsExt};
use axum_extra::{
	headers::{authorization::Bearer, Authorization},
	TypedHeader,
};
use database::models::token_revocations::TokenRevocation;
use tower_cookies::Cookies;

use crate::{error::ServerError, AppState};

use super::token::Claims;

pub struct AuthUser(pub Claims);

impl FromRequestParts<AppState> for AuthUser {
	type Rejection = ServerError;

	async fn from_request_parts(
		parts: &mut Parts,
		state: &AppState,
	) -> Result<Self, Self::Rejection> {
		let token = extract_token(parts).await?;

		let claims = state
			.tokens
			.verify(&token)
			.map_err(|_| ServerError::InvalidToken)?;

		let revoked = TokenRevocation::is_revoked(state.pool, claims.jti)
			.await
			.map_err(|_| ServerError::Internal)?;

		if revoked {
			return Err(ServerError::InvalidToken);
		}

		Ok(AuthUser(claims))
	}
}

async fn extract_token(parts: &mut Parts) -> Result<String, ServerError> {
	if let Ok(TypedHeader(Authorization(bearer))) =
		parts.extract::<TypedHeader<Authorization<Bearer>>>().await
	{
		return Ok(bearer.token().to_string());
	}

	if let Ok(cookies) = parts.extract::<Cookies>().await
		&& let Some(cookie) = cookies.get("session")
	{
		return Ok(cookie.value().to_string());
	}

	Err(ServerError::MissingToken)
}
