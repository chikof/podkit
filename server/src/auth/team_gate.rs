//! One answer for a team that is not yours: 404.
//!
//! Every JSON route under `/teams/{team_id}/` passes through [`gate`] before its handler or
//! its extractors run, so a body that does not parse or a permission that is missing can
//! never tell a stranger that the team exists (403, 400 and 422 would). The checks inside
//! the handlers ([`super::permission::authorize`]) say 404 for a non-member as well, for
//! the callers that do not come through here (the web UI, jobs).

use axum::extract::{FromRequestParts, OriginalUri, Request, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;

use super::AuthUser;
use crate::error::ServerError;
use crate::ids::{TeamId, UserId};
use crate::state::AppState;

/// The team a path under `/api/v1` is about, when it names one.
fn team_of(path: &str) -> Option<TeamId> {
	let path = path.strip_prefix("/api/v1").unwrap_or(path);
	let mut segments = path.split('/').filter(|s| !s.is_empty());
	if segments.next() != Some("teams") {
		return None;
	}
	segments.next()?.parse().ok().map(TeamId)
}

/// Whether `user` belongs to `team`.
///
/// # Errors
/// The database fails.
pub async fn is_member(db: &PgPool, user: UserId, team: TeamId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM team_members WHERE team_id = $1 AND user_id = $2)",
	)
	.bind(team.0)
	.bind(user.0)
	.fetch_one(db)
	.await
}

/// Answers 404 for a signed-in caller who is not a member of the team the path names.
/// A caller who is not signed in is left to the handler, which says 401.
pub async fn gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
	let path = req
		.extensions()
		.get::<OriginalUri>()
		.map_or_else(|| req.uri().path(), |o| o.0.path())
		.to_owned();
	let Some(team) = team_of(&path) else {
		return next.run(req).await;
	};
	let (mut parts, body): (Parts, _) = req.into_parts();
	let user = AuthUser::from_request_parts(&mut parts, &state).await;
	let req = Request::from_parts(parts, body);
	let Ok(user) = user else {
		return next.run(req).await;
	};
	match is_member(&state.db, user.id, team).await {
		Ok(true) => next.run(req).await,
		Ok(false) => ServerError::NotFound.into_response(),
		Err(e) => ServerError::from(e).into_response(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_team_comes_from_the_path() {
		assert_eq!(team_of("/api/v1/teams/7/servers"), Some(TeamId(7)));
		assert_eq!(team_of("/teams/7"), Some(TeamId(7)));
		assert_eq!(team_of("/teams/7/"), Some(TeamId(7)));
		assert_eq!(team_of("/api/v1/teams"), None);
		assert_eq!(team_of("/api/v1/teams/"), None);
		assert_eq!(team_of("/api/v1/teams/abc/x"), None);
		assert_eq!(team_of("/api/v1/templates"), None);
		assert_eq!(team_of("/api/v1/auth/login"), None);
	}
}
