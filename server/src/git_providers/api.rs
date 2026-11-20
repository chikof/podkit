//! Talking to a provider: one [`Target`] per call (the provider's kind, address and decrypted
//! credentials) and one method per thing podkit asks. The provider crates differ in their
//! paths and headers, not in what they answer, so everything here is the same shape.
//!
//! No method puts a credential in an error: a failure says what went wrong in words.

use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Deserialize;

use super::{Credentials, Kind, gitea, github, gitlab};
use crate::error::ServerError;

/// How long one provider call may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// How many repositories a picker page shows.
pub const PAGE_SIZE: usize = 20;

/// A repository a provider offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
	/// `owner/name`.
	pub full_name: String,
	pub default_branch: String,
	pub private: bool,
}

/// One page of repositories.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoPage {
	pub repos: Vec<Repo>,
	/// The next page number, when there is more.
	pub next_page: Option<u32>,
}

/// What a connection test found out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
	/// The account or app the credentials belong to.
	pub login: String,
	/// One line more: how many installations or what the token may do.
	pub detail: String,
}

/// What the agent needs to clone: the plain URL and a token that works now.
#[derive(Clone)]
pub struct CloneAccess {
	pub repo_url: String,
	/// Short-lived for a GitHub App; the stored token for the others.
	pub token: String,
}

/// A provider as the clients see it. No `Debug`: it carries the credentials.
pub struct Target<'a> {
	pub kind: Kind,
	/// The web address of the instance, without a trailing slash.
	pub base_url: &'a str,
	pub credentials: &'a Credentials,
	pub http: &'a reqwest::Client,
}

impl Target<'_> {
	/// Checks the credentials against the provider.
	pub async fn test(&self) -> Result<Identity, ServerError> {
		match self.kind {
			Kind::GithubApp => github::test(self).await,
			Kind::Gitlab => gitlab::test(self).await,
			Kind::Gitea => gitea::test(self).await,
		}
	}

	/// One page of the repositories the credentials can see, matching `query` when it is not
	/// empty. `page` starts at 1.
	pub async fn repos(&self, query: &str, page: u32) -> Result<RepoPage, ServerError> {
		let page = page.max(1);
		match self.kind {
			Kind::GithubApp => github::repos(self, query, page).await,
			Kind::Gitlab => gitlab::repos(self, query, page).await,
			Kind::Gitea => gitea::repos(self, query, page).await,
		}
	}

	/// The branches of `repo`, the default one first.
	pub async fn branches(&self, repo: &str) -> Result<Vec<String>, ServerError> {
		match self.kind {
			Kind::GithubApp => github::branches(self, repo).await,
			Kind::Gitlab => gitlab::branches(self, repo).await,
			Kind::Gitea => gitea::branches(self, repo).await,
		}
	}

	/// What to clone `repo` with: the URL, and a token minted for this clone.
	pub async fn clone_access(&self, repo: &str) -> Result<CloneAccess, ServerError> {
		let repo_url = format!("{}/{repo}.git", self.base_url);
		let token = match self.kind {
			Kind::GithubApp => github::installation_token(self, repo).await?,
			Kind::Gitlab | Kind::Gitea => match self.credentials {
				Credentials::Token { token } => token.clone(),
				Credentials::GithubApp { .. } => return Err(wrong_credentials()),
			},
		};
		Ok(CloneAccess { repo_url, token })
	}

	/// Makes a push webhook on `repo` that calls `url` signed with `secret`. `None` for a
	/// GitHub App, whose one webhook is set when the app is made.
	pub async fn create_hook(
		&self,
		repo: &str,
		url: &str,
		secret: &str,
	) -> Result<Option<String>, ServerError> {
		match self.kind {
			Kind::GithubApp => Ok(None),
			Kind::Gitlab => gitlab::create_hook(self, repo, url, secret).await.map(Some),
			Kind::Gitea => gitea::create_hook(self, repo, url, secret).await.map(Some),
		}
	}

	/// Removes a webhook [`Self::create_hook`] made.
	pub async fn delete_hook(&self, repo: &str, hook: &str) -> Result<(), ServerError> {
		match self.kind {
			Kind::GithubApp => Ok(()),
			Kind::Gitlab => gitlab::delete_hook(self, repo, hook).await,
			Kind::Gitea => gitea::delete_hook(self, repo, hook).await,
		}
	}
}

pub(super) fn wrong_credentials() -> ServerError {
	ServerError::GitProvider("The stored credentials do not fit this kind of provider.".to_owned())
}

/// The host of `base_url`, for messages.
pub(super) fn host(base_url: &str) -> String {
	url::Url::parse(base_url)
		.ok()
		.and_then(|u| u.host_str().map(str::to_owned))
		.unwrap_or_else(|| base_url.to_owned())
}

/// Sends a request and turns a refusal into words. `what` completes "Could not <what>".
pub(super) async fn send(
	req: RequestBuilder,
	base_url: &str,
	what: &str,
) -> Result<Response, ServerError> {
	let res = req.timeout(TIMEOUT).send().await.map_err(|e| {
		tracing::warn!(error = %e.without_url(), "git provider unreachable");
		ServerError::GitProvider(format!("Could not reach {}.", host(base_url)))
	})?;
	let status = res.status();
	if status.is_success() {
		return Ok(res);
	}
	Err(ServerError::GitProvider(match status {
		StatusCode::UNAUTHORIZED => format!(
			"{} rejected the credentials (HTTP 401). Check the token, and that it has not expired.",
			host(base_url)
		),
		StatusCode::FORBIDDEN => format!(
			"{} refused the request (HTTP 403). The token may lack a scope, or the rate limit is used up.",
			host(base_url)
		),
		StatusCode::NOT_FOUND => format!("Could not {what}: {} answered 404.", host(base_url)),
		other => format!(
			"Could not {what}: {} answered HTTP {}.",
			host(base_url),
			other.as_u16()
		),
	}))
}

/// Reads a JSON answer.
pub(super) async fn json<T: for<'de> Deserialize<'de>>(
	res: Response,
	base_url: &str,
) -> Result<T, ServerError> {
	let bytes = res.bytes().await.map_err(|_| {
		ServerError::GitProvider(format!("Lost the connection to {}.", host(base_url)))
	})?;
	serde_json::from_slice(&bytes).map_err(|_| {
		ServerError::GitProvider(format!(
			"{} answered with something podkit does not understand. Is the address right?",
			host(base_url)
		))
	})
}

/// `path` with `params` as its query string, percent-encoded.
pub(super) fn with_query(path: &str, params: &[(&str, &str)]) -> String {
	let mut query = url::form_urlencoded::Serializer::new(String::new());
	for (k, v) in params {
		query.append_pair(k, v);
	}
	format!("{path}?{}", query.finish())
}

/// Sets a JSON body: reqwest's own `json` needs a feature this build leaves out.
pub(super) fn with_json(req: RequestBuilder, body: &serde_json::Value) -> RequestBuilder {
	req.header("content-type", "application/json")
		.body(body.to_string())
}

/// The next page number a `Link` header names, if it has a `rel="next"`.
pub(super) fn next_link(res: &Response) -> Option<u32> {
	let link = res.headers().get(reqwest::header::LINK)?.to_str().ok()?;
	link.split(',')
		.find(|part| part.contains("rel=\"next\""))
		.and_then(|part| {
			let url = part.split(';').next()?.trim().trim_matches(['<', '>']);
			url::Url::parse(url)
				.ok()?
				.query_pairs()
				.find(|(k, _)| k == "page")
				.and_then(|(_, v)| v.parse().ok())
		})
}

/// Percent-encodes one path segment, slashes included (GitLab wants `group%2Frepo`).
pub(super) fn segment(s: &str) -> String {
	url::form_urlencoded::byte_serialize(s.as_bytes())
		.collect::<String>()
		.replace('+', "%20")
}

/// Is `repo` a plausible `owner/name`? Checked before it is put into a path or a clone URL.
#[must_use]
pub fn valid_repo(repo: &str) -> bool {
	let parts: Vec<&str> = repo.split('/').collect();
	parts.len() >= 2
		&& parts.iter().all(|p| {
			!p.is_empty()
				&& *p != "." && *p != ".."
				&& !p.starts_with('-')
				&& p.bytes()
					.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
		})
}
