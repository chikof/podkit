//! GitHub App client. The app signs a short JWT with its private key; that asks GitHub for a
//! token of one installation, and the installation token lists repositories and clones. Both
//! are minted when needed and never stored.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::json;

use super::api::{
	Identity, PAGE_SIZE, Repo, RepoPage, Target, host, json as read_json, send, valid_repo,
	wrong_credentials,
};
use super::{Credentials, jwt};
use crate::error::ServerError;

/// How many pages of one list podkit reads before it stops: 100 each.
const MAX_PAGES: u32 = 10;
const PER_PAGE: usize = 100;

/// The REST API of the GitHub at `base_url`: `api.github.com`, or `/api/v3` of an Enterprise.
#[must_use]
pub fn api_base(base_url: &str) -> String {
	if base_url == "https://github.com" {
		"https://api.github.com".to_owned()
	} else {
		format!("{base_url}/api/v3")
	}
}

pub(super) fn headers(req: RequestBuilder) -> RequestBuilder {
	req.header("accept", "application/vnd.github+json")
		.header("user-agent", "podkit")
		.header("x-github-api-version", "2022-11-28")
}

fn app<'a>(t: &'a Target<'_>) -> Result<(i64, &'a str), ServerError> {
	match t.credentials {
		Credentials::GithubApp {
			app_id,
			private_key,
			..
		} => Ok((*app_id, private_key)),
		Credentials::Token { .. } => Err(wrong_credentials()),
	}
}

fn now() -> i64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A request signed as the app.
fn as_app(
	t: &Target<'_>,
	method: reqwest::Method,
	path: &str,
) -> Result<RequestBuilder, ServerError> {
	let (app_id, key) = app(t)?;
	let token = jwt::app_token(key, app_id, now()).map_err(|_| {
		ServerError::GitProvider(
			"The app's private key could not be read. Connect the app again.".to_owned(),
		)
	})?;
	Ok(headers(
		t.http
			.request(method, format!("{}{path}", api_base(t.base_url)))
			.bearer_auth(token),
	))
}

#[derive(Deserialize)]
struct Account {
	login: String,
}

#[derive(Deserialize)]
struct Installation {
	id: i64,
	account: Option<Account>,
}

#[derive(Deserialize)]
struct AppInfo {
	slug: Option<String>,
	name: Option<String>,
}

/// Every installation of the app.
async fn installations(t: &Target<'_>) -> Result<Vec<Installation>, ServerError> {
	let mut all = Vec::new();
	for page in 1..=MAX_PAGES {
		let req = as_app(
			t,
			reqwest::Method::GET,
			&format!("/app/installations?per_page={PER_PAGE}&page={page}"),
		)?;
		let batch: Vec<Installation> = read_json(
			send(req, t.base_url, "list the installations").await?,
			t.base_url,
		)
		.await?;
		let full = batch.len() == PER_PAGE;
		all.extend(batch);
		if !full {
			break;
		}
	}
	Ok(all)
}

#[derive(Deserialize)]
struct TokenAnswer {
	token: String,
}

/// A token for one installation, valid for an hour. Minted for the one thing that needs it.
async fn token_for(t: &Target<'_>, installation: i64) -> Result<String, ServerError> {
	let post = as_app(
		t,
		reqwest::Method::POST,
		&format!("/app/installations/{installation}/access_tokens"),
	)?;
	let answer: TokenAnswer = read_json(
		send(post, t.base_url, "get an installation token").await?,
		t.base_url,
	)
	.await?;
	Ok(answer.token)
}

/// The installation that holds `repo`, found through the app's own view of it.
async fn installation_of(t: &Target<'_>, repo: &str) -> Result<i64, ServerError> {
	if !valid_repo(repo) {
		return Err(ServerError::Validation(
			"That repository name is not valid.".to_owned(),
		));
	}
	let req = as_app(
		t,
		reqwest::Method::GET,
		&format!("/repos/{repo}/installation"),
	)?;
	let found: Installation = read_json(
		send(req, t.base_url, "find the installation of the repository")
			.await
			.map_err(|e| match e {
				ServerError::GitProvider(m) if m.contains("answered 404") => {
					ServerError::GitProvider(format!(
						"The app is not installed on {repo}. Install it from the provider's page."
					))
				}
				other => other,
			})?,
		t.base_url,
	)
	.await?;
	Ok(found.id)
}

/// A short-lived token that can clone `repo`.
pub(super) async fn installation_token(t: &Target<'_>, repo: &str) -> Result<String, ServerError> {
	token_for(t, installation_of(t, repo).await?).await
}

pub(super) async fn test(t: &Target<'_>) -> Result<Identity, ServerError> {
	let info: AppInfo = read_json(
		send(
			as_app(t, reqwest::Method::GET, "/app")?,
			t.base_url,
			"read the app",
		)
		.await?,
		t.base_url,
	)
	.await?;
	let installs = installations(t).await?;
	let detail = if installs.is_empty() {
		"Not installed anywhere yet: install the app on an account or organization.".to_owned()
	} else {
		let names: Vec<&str> = installs
			.iter()
			.filter_map(|i| i.account.as_ref().map(|a| a.login.as_str()))
			.collect();
		format!("Installed on {}.", names.join(", "))
	};
	Ok(Identity {
		login: info.slug.or(info.name).unwrap_or_else(|| host(t.base_url)),
		detail,
	})
}

#[derive(Deserialize)]
struct RepoRow {
	full_name: String,
	#[serde(default)]
	default_branch: Option<String>,
	#[serde(default)]
	private: bool,
}

#[derive(Deserialize)]
struct RepoList {
	repositories: Vec<RepoRow>,
}

/// Every repository of every installation, filtered by `query`, sorted, then cut to a page.
/// GitHub has no search over what an installation can see, so the list is read in full
/// (at most 1000 repositories per installation).
pub(super) async fn repos(t: &Target<'_>, query: &str, page: u32) -> Result<RepoPage, ServerError> {
	let query = query.trim().to_lowercase();
	let mut all: Vec<Repo> = Vec::new();
	for installation in installations(t).await? {
		let token = token_for(t, installation.id).await?;
		for p in 1..=MAX_PAGES {
			let req = headers(t.http.get(format!(
				"{}/installation/repositories?per_page={PER_PAGE}&page={p}",
				api_base(t.base_url)
			)))
			.bearer_auth(&token);
			let list: RepoList = read_json(
				send(req, t.base_url, "list the repositories").await?,
				t.base_url,
			)
			.await?;
			let full = list.repositories.len() == PER_PAGE;
			all.extend(list.repositories.into_iter().map(|r| Repo {
				full_name: r.full_name,
				default_branch: r.default_branch.unwrap_or_else(|| "main".to_owned()),
				private: r.private,
			}));
			if !full {
				break;
			}
		}
	}
	all.retain(|r| query.is_empty() || r.full_name.to_lowercase().contains(&query));
	all.sort_by_key(|r| r.full_name.to_lowercase());
	all.dedup_by(|a, b| a.full_name == b.full_name);
	let start = (page as usize - 1) * PAGE_SIZE;
	let next_page = (all.len() > start + PAGE_SIZE).then_some(page + 1);
	Ok(RepoPage {
		repos: all.into_iter().skip(start).take(PAGE_SIZE).collect(),
		next_page,
	})
}

#[derive(Deserialize)]
struct Branch {
	name: String,
}

pub(super) async fn branches(t: &Target<'_>, repo: &str) -> Result<Vec<String>, ServerError> {
	let token = installation_token(t, repo).await?;
	let api = api_base(t.base_url);
	let info: RepoRow = read_json(
		send(
			headers(t.http.get(format!("{api}/repos/{repo}"))).bearer_auth(&token),
			t.base_url,
			"read the repository",
		)
		.await?,
		t.base_url,
	)
	.await?;
	let mut names: Vec<String> = Vec::new();
	for p in 1..=MAX_PAGES {
		let req = headers(t.http.get(format!(
			"{api}/repos/{repo}/branches?per_page={PER_PAGE}&page={p}"
		)))
		.bearer_auth(&token);
		let batch: Vec<Branch> = read_json(
			send(req, t.base_url, "list the branches").await?,
			t.base_url,
		)
		.await?;
		let full = batch.len() == PER_PAGE;
		names.extend(batch.into_iter().map(|b| b.name));
		if !full {
			break;
		}
	}
	Ok(default_first(names, info.default_branch.as_deref()))
}

/// Puts the default branch first and keeps the rest in order.
pub(super) fn default_first(mut names: Vec<String>, default: Option<&str>) -> Vec<String> {
	if let Some(default) = default
		&& let Some(i) = names.iter().position(|n| n == default)
	{
		let d = names.remove(i);
		names.insert(0, d);
	}
	names
}

/// What GitHub returns for a manifest code: the app and its secrets.
pub struct Converted {
	pub app_id: i64,
	pub slug: String,
	pub private_key: String,
	pub webhook_secret: String,
}

#[derive(Deserialize)]
struct ConvertedRow {
	id: i64,
	slug: String,
	pem: String,
	#[serde(default)]
	webhook_secret: Option<String>,
}

/// Exchanges the one-time `code` GitHub redirected back with for the app's id, private key and
/// webhook secret. The code is single-use and expires within an hour; no credentials needed.
pub async fn convert_manifest(
	http: &reqwest::Client,
	base_url: &str,
	code: &str,
) -> Result<Converted, ServerError> {
	if code.is_empty()
		|| !code
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
	{
		return Err(ServerError::Validation(
			"GitHub sent back a code that is not valid.".to_owned(),
		));
	}
	let req = headers(http.post(format!(
		"{}/app-manifests/{code}/conversions",
		api_base(base_url)
	)));
	let row: ConvertedRow = read_json(
		send(req, base_url, "finish creating the app").await?,
		base_url,
	)
	.await?;
	Ok(Converted {
		app_id: row.id,
		slug: row.slug,
		private_key: row.pem,
		webhook_secret: row.webhook_secret.unwrap_or_default(),
	})
}

/// The manifest GitHub creates the app from. `hook_url` is `None` when podkit has no public
/// address: the app is made without a webhook then.
#[must_use]
pub fn manifest(
	name: &str,
	home_url: &str,
	redirect_url: &str,
	setup_url: &str,
	hook_url: Option<&str>,
) -> String {
	let mut m = json!({
		"name": name,
		"url": home_url,
		"redirect_url": redirect_url,
		"setup_url": setup_url,
		"setup_on_update": true,
		"public": false,
		"request_oauth_on_install": false,
		"default_permissions": { "contents": "read", "metadata": "read", "checks": "read", "statuses": "write" },
		"default_events": [],
	});
	if let Some(hook) = hook_url {
		m["hook_attributes"] = json!({ "url": hook, "active": true });
		m["default_events"] = json!(["push"]);
	}
	m.to_string()
}
