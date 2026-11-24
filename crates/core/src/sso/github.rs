//! GitHub as an identity provider (OAuth 2.0 web application flow; GitHub speaks no OIDC for
//! user sign-in). Docs, read for this module:
//! - flow and parameters: <https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps>
//! - PKCE support: <https://github.blog/changelog/2025-07-14-pkce-support-for-oauth-and-github-app-authentication/>
//! - the user: <https://docs.github.com/en/rest/users/users#get-the-authenticated-user>
//! - their emails: <https://docs.github.com/en/rest/users/emails#list-email-addresses-for-the-authenticated-user>
//!
//! GitHub Enterprise Server serves the same endpoints from its own host: the web flow from
//! the host itself and the API under `/api/v3`
//! (<https://docs.github.com/en/enterprise-server@latest/rest/overview/resources-in-the-rest-api>).

use std::time::Duration;

use serde::Deserialize;

use super::Identity;
use super::oidc::{Failure, Pkce, form_body, read_json};
use crate::net::outbound::Outbound;

const TIMEOUT: Duration = Duration::from_secs(10);
/// GitHub refuses API calls with no `User-Agent`.
const USER_AGENT: &str = "podkit";
/// `read:user` for the profile, `user:email` for the verified address.
pub const SCOPES: &str = "read:user user:email";

/// Where GitHub's pieces live: github.com, or an Enterprise Server host.
#[derive(Debug, Clone)]
pub struct Endpoints {
	pub web: String,
	pub api: String,
}

pub fn endpoints(base: Option<&str>) -> Endpoints {
	match base
		.map(|b| b.trim_end_matches('/'))
		.filter(|b| !b.is_empty())
	{
		None => Endpoints {
			web: "https://github.com".to_owned(),
			api: "https://api.github.com".to_owned(),
		},
		Some(host) => Endpoints {
			web: host.to_owned(),
			api: format!("{host}/api/v3"),
		},
	}
}

pub fn authorization_url(
	ep: &Endpoints,
	client_id: &str,
	redirect_uri: &str,
	state: &str,
	pkce: &Pkce,
) -> Result<String, Failure> {
	let mut url = url::Url::parse(&format!("{}/login/oauth/authorize", ep.web))
		.map_err(|e| format!("bad GitHub address: {e}"))?;
	url.query_pairs_mut()
		.append_pair("client_id", client_id)
		.append_pair("redirect_uri", redirect_uri)
		.append_pair("scope", SCOPES)
		.append_pair("state", state)
		// Accounts are made by podkit's own policy, not by GitHub's sign-up page.
		.append_pair("allow_signup", "false")
		.append_pair("code_challenge", &pkce.challenge)
		.append_pair("code_challenge_method", "S256");
	Ok(url.into())
}

#[derive(Deserialize)]
struct TokenAnswer {
	access_token: Option<String>,
	error: Option<String>,
	error_description: Option<String>,
}

/// Trades the code for an access token. GitHub answers 200 with an `error` field when
/// the code is bad.
pub async fn exchange(
	http: &Outbound,
	ep: &Endpoints,
	client_id: &str,
	client_secret: &str,
	redirect_uri: &str,
	code: &str,
	verifier: &str,
) -> Result<String, Failure> {
	let response = http
		.post(&format!("{}/login/oauth/access_token", ep.web))
		.await
		.map_err(|d| format!("cannot reach GitHub: {d} ({})", d.reason()))?
		.header("accept", "application/json")
		.timeout(TIMEOUT)
		.header("content-type", "application/x-www-form-urlencoded")
		.body(form_body(&[
			("client_id", client_id),
			("client_secret", client_secret),
			("code", code),
			("redirect_uri", redirect_uri),
			("code_verifier", verifier),
		]))
		.send()
		.await
		.map_err(|e| format!("cannot reach GitHub: {e}"))?;
	if !response.status().is_success() {
		return Err(format!(
			"GitHub's token endpoint answered {}",
			response.status()
		));
	}
	let answer: TokenAnswer = read_json(response, "GitHub's token endpoint").await?;
	match (answer.access_token, answer.error) {
		(Some(token), None) if !token.is_empty() => Ok(token),
		(_, Some(error)) => Err(format!(
			"GitHub refused the code: {error} {}",
			answer.error_description.unwrap_or_default()
		)),
		_ => Err("GitHub returned no access token".to_owned()),
	}
}

#[derive(Deserialize)]
struct User {
	id: i64,
	login: String,
	name: Option<String>,
}

#[derive(Deserialize)]
#[allow(clippy::struct_field_names)]
struct Email {
	email: String,
	primary: bool,
	verified: bool,
}

async fn api<T: for<'de> Deserialize<'de>>(
	http: &Outbound,
	ep: &Endpoints,
	token: &str,
	path: &str,
) -> Result<T, Failure> {
	let response = http
		.get(&format!("{}{path}", ep.api))
		.await
		.map_err(|d| format!("cannot reach the GitHub API: {d} ({})", d.reason()))?
		.bearer_auth(token)
		.header("accept", "application/vnd.github+json")
		.header("x-github-api-version", "2022-11-28")
		.header("user-agent", USER_AGENT)
		.timeout(TIMEOUT)
		.send()
		.await
		.map_err(|e| format!("cannot reach the GitHub API: {e}"))?;
	if !response.status().is_success() {
		return Err(format!(
			"GitHub's API answered {} for {path}",
			response.status()
		));
	}
	read_json(response, path).await
}

/// Who the access token belongs to: the numeric id (which does not change when the person
/// renames themselves) and their primary email, which counts as verified only when
/// GitHub says so.
pub async fn identity(http: &Outbound, ep: &Endpoints, token: &str) -> Result<Identity, Failure> {
	let user: User = api(http, ep, token, "/user").await?;
	let emails: Vec<Email> = api(http, ep, token, "/user/emails").await?;
	let primary = emails
		.into_iter()
		.find(|e| e.primary)
		.filter(|e| e.verified);
	Ok(Identity {
		subject: user.id.to_string(),
		email: primary.map(|e| e.email),
		email_verified: true,
		name: user.name.filter(|n| !n.is_empty()).unwrap_or(user.login),
		groups: Vec::new(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn oidc_github_enterprise_uses_the_host_and_api_v3() {
		let com = endpoints(None);
		assert_eq!(com.web, "https://github.com");
		assert_eq!(com.api, "https://api.github.com");
		let ghe = endpoints(Some("https://git.corp.example/"));
		assert_eq!(ghe.web, "https://git.corp.example");
		assert_eq!(ghe.api, "https://git.corp.example/api/v3");
	}
}
