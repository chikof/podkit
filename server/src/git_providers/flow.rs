//! Connecting a GitHub App through GitHub's app manifest flow.
//!
//! 1. [`start`] builds the manifest and a signed `state`; the browser posts both to GitHub.
//! 2. GitHub shows its own "create app" page and redirects back to
//!    `/git-providers/github/callback?code=..&state=..`.
//! 3. [`finish`] checks the state, exchanges the code for the app's id, private key and
//!    webhook secret, and stores the provider.
//! 4. GitHub's install page then redirects to `/git-providers/github/installed`, which
//!    [`installed`] checks the same way.
//!
//! The state is the whole defence against a forged callback: it is age-encrypted by podkit (so
//! nobody else can make one), names the user who started the flow (so only that signed-in user
//! can finish it), and expires in an hour.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use super::service::{self, ProviderView};
use super::{Kind, github};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{GitProviderId, TeamId, UserId};
use crate::state::AppState;

/// How long a started flow may take to come back.
const LIFETIME_SECS: i64 = 60 * 60;

/// Path of the redirect GitHub sends the browser to with the code.
pub const CALLBACK_PATH: &str = "/git-providers/github/callback";
/// Path of the redirect after the app is installed.
pub const INSTALLED_PATH: &str = "/git-providers/github/installed";
/// Path GitHub's push webhook calls, followed by the provider id.
pub const WEBHOOK_PREFIX: &str = "/git-providers/github/webhook/";

#[derive(Serialize, Deserialize)]
struct FlowState {
	team: i64,
	user: i64,
	provider: i64,
	name: String,
	base_url: String,
	exp: i64,
	nonce: String,
}

fn now() -> i64 {
	time::OffsetDateTime::now_utc().unix_timestamp()
}

fn seal_state(s: &AppState, state: &FlowState) -> AppResult<String> {
	let json = serde_json::to_string(state).map_err(|_| ServerError::Internal)?;
	let sealed = s
		.secrets
		.encrypt(&json)
		.map_err(|_| ServerError::Internal)?;
	Ok(URL_SAFE_NO_PAD.encode(sealed))
}

/// Opens a state for `user`: it must be ours, unexpired, and made by this user.
fn open_state(s: &AppState, user: UserId, raw: &str) -> AppResult<FlowState> {
	let invalid = || {
		ServerError::Validation(
			"This GitHub link is not valid or has expired. Start again from Git providers."
				.to_owned(),
		)
	};
	let bytes = URL_SAFE_NO_PAD.decode(raw.trim()).map_err(|_| invalid())?;
	let json = s.secrets.decrypt(&bytes).map_err(|_| invalid())?;
	let state: FlowState = serde_json::from_str(&json).map_err(|_| invalid())?;
	if state.exp < now() || state.user != user.0 {
		return Err(invalid());
	}
	Ok(state)
}

/// The site the browser used to reach podkit, e.g. `https://podkit.example`: the proxy's
/// forwarded host and protocol when it sets them, else `Host` over plain HTTP.
#[must_use]
pub fn origin(headers: &axum::http::HeaderMap) -> String {
	let first = |name: &str| {
		headers
			.get(name)
			.and_then(|v| v.to_str().ok())
			.and_then(|v| v.split(',').next())
			.map(str::trim)
			.filter(|v| !v.is_empty())
	};
	let proto = match first("x-forwarded-proto") {
		Some("https") => "https",
		_ => "http",
	};
	let host = first("x-forwarded-host")
		.or_else(|| first("host"))
		.unwrap_or("localhost");
	format!("{proto}://{host}")
}

/// What the browser posts to GitHub.
#[derive(Debug, Clone)]
pub struct Start {
	/// Where the form goes: GitHub's "new app" page, with the state in its query.
	pub action: String,
	/// The manifest, a JSON document, as the `manifest` field.
	pub manifest: String,
}

/// Where the app is created: the user's own account, or an organization's.
#[derive(Debug, Clone, Default)]
pub struct StartInput {
	pub name: String,
	/// An organization to create the app under; the user's own account when empty.
	pub organization: String,
	/// A GitHub Enterprise Server address; github.com when empty.
	pub base_url: String,
}

/// Builds the manifest form. `origin` is the address the browser used to reach podkit; it
/// only has to work for the browser's redirects. Needs `create git_provider`.
pub async fn start(
	s: &AppState,
	user: UserId,
	team: TeamId,
	origin: &str,
	input: StartInput,
) -> AppResult<Start> {
	authorize(&s.db, user, team, Action::Create, Resource::GitProvider).await?;
	let name = input.name.trim();
	if name.is_empty() || name.chars().count() > 34 || name.chars().any(char::is_control) {
		return Err(ServerError::Validation(
			"The app's name must be 1 to 34 characters; GitHub requires a unique one.".to_owned(),
		));
	}
	let base_url = if input.base_url.trim().is_empty() {
		Kind::GithubApp
			.default_base_url()
			.unwrap_or_default()
			.to_owned()
	} else {
		service::normalize_base_url(&input.base_url)?
	};
	let organization = input.organization.trim();
	if !organization.is_empty()
		&& !organization
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
	{
		return Err(ServerError::Validation(
			"That is not a GitHub organization name.".to_owned(),
		));
	}
	let provider = GitProviderId::generate();
	let state = seal_state(
		s,
		&FlowState {
			team: team.0,
			user: user.0,
			provider: provider.0,
			name: name.to_owned(),
			base_url: base_url.clone(),
			exp: now() + LIFETIME_SECS,
			nonce: crypto::generate_token(12),
		},
	)?;
	let origin = origin.trim_end_matches('/');
	let hook = s
		.config
		.public_url
		.as_deref()
		.map(|public| format!("{public}{WEBHOOK_PREFIX}{provider}"));
	let manifest = github::manifest(
		name,
		origin,
		&format!("{origin}{CALLBACK_PATH}"),
		&format!("{origin}{INSTALLED_PATH}?state={state}"),
		hook.as_deref(),
	);
	let page = if organization.is_empty() {
		format!("{base_url}/settings/apps/new")
	} else {
		format!("{base_url}/organizations/{organization}/settings/apps/new")
	};
	Ok(Start {
		action: format!("{page}?state={state}"),
		manifest,
	})
}

/// The provider a finished flow stored.
pub struct Finished {
	pub provider: ProviderView,
	/// Where to send the browser to install the app.
	pub install_url: String,
}

/// Finishes the flow: checks `state`, exchanges `code`, stores the app. `user` is the signed-in
/// user making the request.
pub async fn finish(
	s: &AppState,
	user: UserId,
	code: &str,
	raw_state: &str,
) -> AppResult<Finished> {
	let state = open_state(s, user, raw_state)?;
	let team = TeamId(state.team);
	authorize(&s.db, user, team, Action::Create, Resource::GitProvider).await?;
	service::vet_base_url(s, &state.base_url).await?;
	let converted =
		github::convert_manifest(s.outbound.vetted_client(), &state.base_url, code).await?;
	let provider = service::create_github(
		s,
		user,
		team,
		GitProviderId(state.provider),
		&state.name,
		&state.base_url,
		&converted,
	)
	.await?;
	let install_url = format!(
		"{}/apps/{}/installations/new?state={}",
		provider.base_url,
		provider.app_slug.as_deref().unwrap_or_default(),
		raw_state.trim()
	);
	Ok(Finished {
		provider,
		install_url,
	})
}

/// A well-formed state of `user` that ran out an hour ago.
#[cfg(test)]
pub(super) fn stale_state_for_tests(s: &AppState, user: UserId, team: TeamId) -> String {
	seal_state(
		s,
		&FlowState {
			team: team.0,
			user: user.0,
			provider: 1,
			name: "old".to_owned(),
			base_url: "http://127.0.0.1:1".to_owned(),
			exp: now() - 1,
			nonce: String::new(),
		},
	)
	.unwrap()
}

/// After the install page: checks the state and says which team to show.
pub fn installed(s: &AppState, user: UserId, raw_state: &str) -> AppResult<TeamId> {
	Ok(TeamId(open_state(s, user, raw_state)?.team))
}
