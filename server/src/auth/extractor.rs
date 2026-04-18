use axum::{extract::FromRequestParts, http::request::Parts, RequestPartsExt};
use axum_extra::{
	headers::{authorization::Bearer, Authorization},
	TypedHeader,
};

use crate::{error::ServerError, AppState};

use super::token::Claims;

pub struct AuthUser(pub Claims);

impl FromRequestParts<AppState> for AuthUser {
	type Rejection = ServerError;

	async fn from_request_parts(
		parts: &mut Parts,
		state: &AppState,
	) -> Result<Self, Self::Rejection> {
		let TypedHeader(Authorization(bearer)) = parts
			.extract::<TypedHeader<Authorization<Bearer>>>()
			.await
			.map_err(|_| ServerError::MissingToken)?;

		let claims = state
			.tokens
			.verify(bearer.token())
			.map_err(|_| ServerError::InvalidToken)?;

		Ok(AuthUser(claims))
	}
}
