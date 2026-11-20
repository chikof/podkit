//! The actions of the Git providers page, and the two redirects GitHub sends the browser
//! back through. Each goes through the provider service, which authorizes and scopes it; a
//! forged POST gets the same refusal as the API.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::response::{IntoResponse, Redirect, Response};
use datastar::axum::ReadSignals;
use serde::Deserialize;
use serde_json::json;
use tower_cookies::Cookies;

use super::{dialogs, providers_url};
use crate::git_providers::Kind;
use crate::git_providers::flow::{self, StartInput};
use crate::git_providers::service::{self, NewTokenProvider};
use crate::ids::{GitProviderId, TeamId};
use crate::state::AppState;
use crate::ui::components::Tone;
use crate::ui::extract::{TeamCtx, UiUser};
use crate::ui::{error, shell, sse};

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the signals the dialogs bind"
)]
pub(super) struct GithubSignals {
	gh_name: String,
	gh_org: String,
	gh_url: String,
}

/// Builds the manifest and patches in the form that sends the browser to GitHub.
pub(super) async fn github_start(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	ReadSignals(sig): ReadSignals<GithubSignals>,
) -> Response {
	let start = flow::start(
		&s,
		ctx.user.id,
		ctx.team.id,
		&flow::origin(&headers),
		StartInput {
			name: sig.gh_name,
			organization: sig.gh_org,
			base_url: sig.gh_url,
		},
	)
	.await;
	match start {
		Ok(start) => sse::patch(&dialogs::github_go(&start.action, &start.manifest))
			.and(sse::signals(&json!({ "_gh_ready": true, "_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the signals the dialogs bind"
)]
pub(super) struct GitlabSignals {
	gl_name: String,
	gl_url: String,
	gl_token: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the signals the dialogs bind"
)]
pub(super) struct GiteaSignals {
	ge_name: String,
	ge_url: String,
	ge_token: String,
}

async fn connect(
	s: &AppState,
	ctx: &TeamCtx,
	cookies: &Cookies,
	kind: Kind,
	name: String,
	base_url: String,
	token: String,
) -> Response {
	let new = NewTokenProvider {
		kind,
		name,
		base_url: Some(base_url),
		token,
	};
	match service::create_token(s, ctx.user.id, ctx.team.id, new).await {
		Ok(provider) => {
			shell::flash(
				cookies,
				Tone::Success,
				&format!("Connected {}", provider.name),
			);
			// The token has done its job; the page reloads without it.
			sse::signals(&json!({ "gl_token": "", "ge_token": "" }))
				.and(sse::redirect(&providers_url(ctx.team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

pub(super) async fn connect_gitlab(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<GitlabSignals>,
) -> Response {
	connect(
		&s,
		&ctx,
		&cookies,
		Kind::Gitlab,
		sig.gl_name,
		sig.gl_url,
		sig.gl_token,
	)
	.await
}

pub(super) async fn connect_gitea(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<GiteaSignals>,
) -> Response {
	connect(
		&s,
		&ctx,
		&cookies,
		Kind::Gitea,
		sig.ge_name,
		sig.ge_url,
		sig.ge_token,
	)
	.await
}

/// Asks the provider who the credentials are and says so in a toast.
pub(super) async fn test(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, GitProviderId)>,
) -> Response {
	match service::test(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(who) => {
			sse::toast(&format!("Connected as {}. {}", who.login, who.detail)).into_response()
		}
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

pub(super) async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, GitProviderId)>,
) -> Response {
	match service::remove(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Git provider deleted");
			sse::redirect(&providers_url(ctx.team.id)).into_response()
		}
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Callback {
	code: String,
	state: String,
}

/// Where GitHub sends the browser after the app is created, with the one-time `code`. The
/// `state` must be one this signed-in user started; anything else is refused before the code
/// is looked at. Then on to GitHub's install page.
pub(super) async fn github_callback(
	State(s): State<AppState>,
	user: UiUser,
	cookies: Cookies,
	Query(q): Query<Callback>,
) -> Response {
	match flow::finish(&s, user.id, &q.code, &q.state).await {
		Ok(done) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!(
					"GitHub App {} created. Install it to choose the repositories podkit may read.",
					done.provider.name
				),
			);
			Redirect::to(&done.install_url).into_response()
		}
		Err(e) => error::respond(&Method::GET, Some(&user), &e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Installed {
	state: String,
}

/// Where GitHub sends the browser after the app is installed.
pub(super) async fn github_installed(
	State(s): State<AppState>,
	user: UiUser,
	cookies: Cookies,
	Query(q): Query<Installed>,
) -> Response {
	match flow::installed(&s, user.id, &q.state) {
		Ok(team) => {
			shell::flash(&cookies, Tone::Success, "GitHub App installed");
			Redirect::to(&providers_url(team)).into_response()
		}
		Err(_) => Redirect::to("/").into_response(),
	}
}
