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
use crate::environments::{self, Environment};
use crate::error::ServerError;
use crate::ids::{
	ApplicationId, DatabaseId, EnvironmentId, ModelId, ProjectId, StackId, TeamId, UserId,
};
use crate::projects::{self, Project};
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
	/// Hash of their picture, when they have one.
	pub avatar: Option<String>,
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
		let header = match users::header_of(&state.db, id).await {
			Ok(Some(header)) => header,
			Ok(None) => return Err(to_login(parts)),
			Err(e) => return Err(error::respond(&parts.method, None, &e.into())),
		};
		Ok(Self {
			id,
			name: header.name,
			avatar: header.avatar,
		})
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
	/// The request path, so the sidebar can mark the current page.
	pub path: String,
	/// The project (and environment) the page is about, when its path names one the
	/// user may read; the header's switchers are built from it.
	pub scope: Option<ProjectScope>,
	/// Events the user has not read yet, for the bell. Counted on page loads only.
	pub unread: i64,
}

/// A project page's place in the hierarchy: the project, its siblings, and the
/// environment in view (`?environment=<id>`, else the project's first).
#[derive(Debug, Clone)]
pub struct ProjectScope {
	pub project: Project,
	pub projects: Vec<Project>,
	pub environments: Vec<Environment>,
	pub environment: Environment,
}

/// The resource a page is about, named by its path parameter.
#[derive(Debug, Clone, Copy)]
enum Resource {
	App(ApplicationId),
	Database(DatabaseId),
	Stack(StackId),
	Model(ModelId),
}

impl Resource {
	/// The environment the resource lives in; `None` when it is not found (the page's
	/// own handler reports that).
	async fn environment(
		self,
		state: &AppState,
		team: TeamId,
		project: ProjectId,
	) -> Option<EnvironmentId> {
		use crate::scope;
		let db = &state.db;
		match self {
			Self::App(id) => scope::application(db, team, project, id)
				.await
				.ok()
				.map(|r| r.environment_id),
			Self::Database(id) => scope::database(db, team, project, id)
				.await
				.ok()
				.map(|r| r.environment_id),
			Self::Stack(id) => scope::stack(db, team, project, id)
				.await
				.ok()
				.map(|r| r.environment_id),
			Self::Model(id) => scope::model(db, team, project, id)
				.await
				.ok()
				.map(|r| r.environment_id),
		}
	}
}

impl ProjectScope {
	/// Best effort: a failure leaves the header without switchers, and the page's own
	/// handler reports the real error. Only navigations pay for it.
	async fn load(
		parts: &Parts,
		state: &AppState,
		user: UserId,
		team: TeamId,
		project: Option<ProjectId>,
		env_slug: Option<&str>,
		resource: Option<Resource>,
	) -> Option<Self> {
		// Datastar fragments (polls, previews) never render the header.
		if !is_navigation(&parts.method) || parts.headers.contains_key("datastar-request") {
			return None;
		}
		let project_id = project?;
		let wanted = parts.uri.query().and_then(|q| {
			url::form_urlencoded::parse(q.as_bytes())
				.find(|(k, _)| k == "environment")
				.and_then(|(_, v)| v.parse::<i64>().ok())
				.map(EnvironmentId)
		});
		let project = projects::service::find(&state.db, user, team, project_id)
			.await
			.ok()?;
		let projects = projects::service::list(&state.db, user, team)
			.await
			.ok()?
			.into_iter()
			.map(|s| s.project)
			.collect();
		let environments = environments::service::list(&state.db, user, team, project_id)
			.await
			.ok()?;
		// The path names the environment (`/projects/{p}/{env_slug}`) on an environment's
		// own page; `?environment=<id>` picks it on the project's other pages.
		// A resource's own pages sit in the environment the resource lives in.
		let resource_environment = match (resource, wanted) {
			(Some(resource), None) => resource.environment(state, team, project_id).await,
			_ => None,
		};
		let environment = environments
			.iter()
			.find(|e| env_slug.is_some_and(|slug| e.slug == slug))
			.or_else(|| environments.iter().find(|e| Some(e.id) == wanted))
			.or_else(|| {
				environments
					.iter()
					.find(|e| Some(e.id) == resource_environment)
			})
			.or_else(|| environments.first())?
			.clone();
		Some(Self {
			project,
			projects,
			environments,
			environment,
		})
	}
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

		let (team_id, project_id, env_slug, resource) =
			match RawPathParams::from_request_parts(parts, state).await {
				Ok(params) => {
					let id = |name: &str| {
						params
							.iter()
							.find(|(k, _)| *k == name)
							.and_then(|(_, v)| v.parse::<i64>().ok())
					};
					let slug = params
						.iter()
						.find(|(k, _)| *k == "env_slug")
						.map(|(_, v)| v.to_owned());
					(
						id("team_id"),
						id("project_id").map(ProjectId),
						slug,
						id("app_id")
							.map(|n| Resource::App(ApplicationId(n)))
							.or_else(|| {
								id("database_id").map(|n| Resource::Database(DatabaseId(n)))
							})
							.or_else(|| id("stack_id").map(|n| Resource::Stack(StackId(n))))
							.or_else(|| id("model_id").map(|n| Resource::Model(ModelId(n)))),
					)
				}
				Err(_) => (None, None, None, None),
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

		let path = parts.uri.path().to_owned();
		let scope = ProjectScope::load(
			parts,
			state,
			user.id,
			team_id,
			project_id,
			env_slug.as_deref(),
			resource,
		)
		.await;

		let unread = if is_navigation(&parts.method) {
			crate::notifications::service::unread_count(&state.db, user.id, team_id)
				.await
				.unwrap_or(0)
		} else {
			0
		};

		Ok(Self {
			user,
			team,
			perms,
			teams,
			path,
			scope,
			unread,
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
