//! Request extractors for the web UI. They differ from the API's `AuthUser`
//! in what a failure looks like: a navigation is redirected to the sign-in
//! page or shown an error page, a Datastar action gets an SSE message.

use std::fmt::Write as _;

use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::{Method, request::Parts};
use axum::response::{IntoResponse, Redirect, Response};

use super::{error, sse};
use crate::auth::permission::{self, Permissions};
use crate::auth::{SessionToken, session, users};
use crate::error::ServerError;
use crate::ids::{TeamId, UserId};
use crate::state::AppState;
use crate::teams::{self, Team};

fn is_navigation(method: &Method) -> bool {
	method == Method::GET || method == Method::HEAD
}

/// Percent-encodes everything but unreserved characters, for a query value.
fn encode_component(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for b in s.bytes() {
		if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
			out.push(char::from(b));
		} else {
			let _ = write!(out, "%{b:02X}");
		}
	}
	out
}

/// Where to send someone without a session. A navigation comes back to where
/// it was after sign-in; an action just opens the sign-in page.
fn to_login(parts: &Parts) -> Response {
	if !is_navigation(&parts.method) {
		return sse::redirect("/login").into_response();
	}
	let target = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
	if target == "/" {
		Redirect::to("/login").into_response()
	} else {
		Redirect::to(&format!("/login?next={}", encode_component(target))).into_response()
	}
}

/// The signed-in user behind the `session` cookie.
#[derive(Debug, Clone)]
pub struct UiUser {
	pub id: UserId,
	pub name: String,
}

impl FromRequestParts<AppState> for UiUser {
	type Rejection = Response;

	async fn from_request_parts(
		parts: &mut Parts,
		state: &AppState,
	) -> Result<Self, Self::Rejection> {
		let Ok(SessionToken(token)) = SessionToken::from_request_parts(parts, state).await else {
			return Err(to_login(parts));
		};
		let id = match session::resolve(&state.db, &token).await {
			Ok(Some(id)) => id,
			Ok(None) => return Err(to_login(parts)),
			Err(e) => return Err(error::respond(&parts.method, None, &e.into())),
		};
		let name = match users::name_of(&state.db, id).await {
			Ok(Some(name)) => name,
			Ok(None) => return Err(to_login(parts)),
			Err(e) => return Err(error::respond(&parts.method, None, &e.into())),
		};
		Ok(Self { id, name })
	}
}

/// The signed-in user inside one of their teams, with what they may do there.
/// Someone outside the team gets the 404 page, exactly as if it didn't exist.
#[derive(Debug, Clone)]
pub struct TeamCtx {
	pub user: UiUser,
	pub team: Team,
	pub perms: Permissions,
	/// Every team the user belongs to, for the switcher.
	pub teams: Vec<Team>,
}

impl FromRequestParts<AppState> for TeamCtx {
	type Rejection = Response;

	async fn from_request_parts(
		parts: &mut Parts,
		state: &AppState,
	) -> Result<Self, Self::Rejection> {
		let user = UiUser::from_request_parts(parts, state).await?;
		let fail = |parts: &Parts, user: &UiUser, e: ServerError| {
			error::respond(&parts.method, Some(user), &e)
		};

		let team_id = match RawPathParams::from_request_parts(parts, state).await {
			Ok(params) => params
				.iter()
				.find(|(k, _)| *k == "team_id")
				.and_then(|(_, v)| v.parse::<i64>().ok()),
			Err(_) => None,
		};
		let Some(team_id) = team_id.map(TeamId) else {
			return Err(fail(parts, &user, ServerError::NotFound));
		};

		let perms = permission::granted(&state.db, user.id, team_id)
			.await
			.map_err(|e| fail(parts, &user, e))?;
		if !perms.is_member() {
			return Err(fail(parts, &user, ServerError::NotFound));
		}
		let teams = teams::list_for_user(&state.db, user.id)
			.await
			.map_err(|e| fail(parts, &user, e.into()))?;
		let Some(team) = teams.iter().find(|t| t.id == team_id).cloned() else {
			return Err(fail(parts, &user, ServerError::NotFound));
		};

		Ok(Self {
			user,
			team,
			perms,
			teams,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn encodes_a_path_and_query_for_next() {
		assert_eq!(
			encode_component("/teams/1/projects"),
			"%2Fteams%2F1%2Fprojects"
		);
		assert_eq!(encode_component("/a?b=c&d"), "%2Fa%3Fb%3Dc%26d");
	}
}
