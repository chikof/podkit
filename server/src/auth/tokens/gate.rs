//! The gate in front of the JSON API: resolves a `pk_` bearer token and decides, from the
//! method and path alone, whether the token may make this request. Requests with any other
//! credential (a session) pass through untouched.

use axum::extract::{OriginalUri, Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::{Grant, PREFIX, Scope, service};
use crate::error::ServerError;
use crate::ids::ProjectId;
use crate::state::AppState;

/// Segments after which a `POST` is a deploy rather than a write.
const DEPLOY_VERBS: [&str; 8] = [
	"deploy",
	"deployments",
	"stop",
	"restart",
	"start",
	"rollback",
	"cancel",
	"bootstrap",
];

pub async fn gate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
	let Some(token) = bearer(&req).filter(|t| t.starts_with(PREFIX)) else {
		return next.run(req).await;
	};
	let path = req
		.extensions()
		.get::<OriginalUri>()
		.map_or_else(|| req.uri().path(), |o| o.0.path())
		.to_owned();
	let path = path.strip_prefix("/api/v1").unwrap_or(&path);
	let method = req.method().clone();
	let result = async {
		let grant = service::resolve(&state.db, &token).await?;
		check(&grant, &method, path)?;
		Ok::<_, ServerError>(grant)
	}
	.await;
	match result {
		Ok(grant) => {
			req.extensions_mut().insert(grant);
			next.run(req).await
		}
		Err(e) => e.into_response(),
	}
}

fn bearer(req: &Request) -> Option<String> {
	let value = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
	let (scheme, token) = value.split_once(' ')?;
	scheme
		.eq_ignore_ascii_case("bearer")
		.then(|| token.trim().to_owned())
}

/// The scope a request needs, from its method and last path segment.
pub fn required_scope(method: &Method, segments: &[&str]) -> Scope {
	if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
		Scope::Read
	} else if method == Method::DELETE {
		Scope::Delete
	} else if segments.last() == Some(&"plan") {
		// A plan compares and changes nothing, so reading is enough to ask for one.
		Scope::Read
	} else if segments.last().is_some_and(|s| DEPLOY_VERBS.contains(s)) {
		Scope::Deploy
	} else {
		Scope::Write
	}
}

fn id(segment: Option<&&str>) -> Option<i64> {
	segment.and_then(|s| s.parse().ok())
}

/// Applies a grant to a request. `path` is relative to `/api/v1`. Order matters: another
/// team's or an excluded project's path is a 404 before scope is considered, so a token
/// cannot probe what it may not see.
pub fn check(grant: &Grant, method: &Method, path: &str) -> Result<(), ServerError> {
	let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
	let first = segments.first().copied().unwrap_or_default();

	// Signing in and out belongs to people, not tokens.
	if first == "auth" {
		return Err(ServerError::Forbidden);
	}

	let in_team = first == "teams" && segments.len() >= 2;
	if in_team && id(segments.get(1)) != Some(grant.team.0) {
		return Err(ServerError::NotFound);
	}

	let in_project = in_team && segments.get(2) == Some(&"projects") && segments.len() >= 4;
	if in_project {
		match id(segments.get(3)) {
			Some(p) if grant.sees(ProjectId(p)) => {}
			_ => return Err(ServerError::ProjectNotFound),
		}
	} else if grant.is_limited() && !limited_may_reach(method, &segments) {
		return Err(ServerError::Forbidden);
	}

	let need = match (first, segments.get(2), method) {
		// Tokens manage tokens, teams get made and members change, only with `root`.
		("teams", Some(&"api-tokens"), _) => Scope::Root,
		("teams", Some(&"members"), m) if m != Method::GET => Scope::Root,
		("teams", None, m) if m != Method::GET => Scope::Root,
		_ => required_scope(method, &segments),
	};
	if grant.allows(need) {
		Ok(())
	} else {
		Err(ServerError::MissingScope(need.as_str()))
	}
}

/// What a project-limited token may still read outside its projects: the lists it needs
/// to find them, and the public catalogs.
fn limited_may_reach(method: &Method, segments: &[&str]) -> bool {
	if method != Method::GET {
		return false;
	}
	matches!(
		segments,
		["teams"] | ["teams", _, "projects" | "servers"] | ["templates" | "catalog", ..]
	)
}
