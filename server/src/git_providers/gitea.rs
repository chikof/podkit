//! Gitea (and Forgejo) client: an access token against `{base}/api/v1`.

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::json;

use super::Credentials;
use super::api::{
	Identity, PAGE_SIZE, Repo, RepoPage, Target, json as read_json, next_link, send, valid_repo,
	with_json, with_query, wrong_credentials,
};
use super::github::default_first;
use crate::error::ServerError;

pub(super) fn req(
	t: &Target<'_>,
	method: reqwest::Method,
	path: &str,
) -> Result<RequestBuilder, ServerError> {
	let Credentials::Token { token } = t.credentials else {
		return Err(wrong_credentials());
	};
	Ok(t.http
		.request(method, format!("{}/api/v1{path}", t.base_url))
		.header("authorization", format!("token {token}"))
		.header("user-agent", "podkit"))
}

pub(super) fn checked(repo: &str) -> Result<&str, ServerError> {
	if valid_repo(repo) {
		Ok(repo)
	} else {
		Err(ServerError::Validation(
			"That repository name is not valid.".to_owned(),
		))
	}
}

#[derive(Deserialize)]
struct User {
	login: String,
}

pub(super) async fn test(t: &Target<'_>) -> Result<Identity, ServerError> {
	let user: User = read_json(
		send(
			req(t, reqwest::Method::GET, "/user")?,
			t.base_url,
			"read the account",
		)
		.await?,
		t.base_url,
	)
	.await?;
	Ok(Identity {
		detail: format!("Signed in as {}.", user.login),
		login: user.login,
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
struct Search {
	data: Vec<RepoRow>,
}

pub(super) async fn repos(t: &Target<'_>, query: &str, page: u32) -> Result<RepoPage, ServerError> {
	let r = req(
		t,
		reqwest::Method::GET,
		&with_query(
			"/repos/search",
			&[
				("q", query.trim()),
				("private", "true"),
				("sort", "updated"),
				("order", "desc"),
				("limit", &PAGE_SIZE.to_string()),
				("page", &page.to_string()),
			],
		),
	)?;
	let res = send(r, t.base_url, "list the repositories").await?;
	let link_next = next_link(&res);
	let rows: Search = read_json(res, t.base_url).await?;
	// Without a `Link` header, a full page is taken to have a successor.
	let next_page = link_next.or_else(|| (rows.data.len() == PAGE_SIZE).then_some(page + 1));
	Ok(RepoPage {
		repos: rows
			.data
			.into_iter()
			.map(|r| Repo {
				full_name: r.full_name,
				default_branch: r.default_branch.unwrap_or_else(|| "main".to_owned()),
				private: r.private,
			})
			.collect(),
		next_page,
	})
}

#[derive(Deserialize)]
struct Branch {
	name: String,
}

pub(super) async fn branches(t: &Target<'_>, repo: &str) -> Result<Vec<String>, ServerError> {
	let repo = checked(repo)?;
	let info: RepoRow = read_json(
		send(
			req(t, reqwest::Method::GET, &format!("/repos/{repo}"))?,
			t.base_url,
			"read the repository",
		)
		.await?,
		t.base_url,
	)
	.await?;
	let mut names = Vec::new();
	for page in 1..=10 {
		let r = req(
			t,
			reqwest::Method::GET,
			&with_query(
				&format!("/repos/{repo}/branches"),
				&[("limit", "50"), ("page", &page.to_string())],
			),
		)?;
		let batch: Vec<Branch> =
			read_json(send(r, t.base_url, "list the branches").await?, t.base_url).await?;
		let full = batch.len() == 50;
		names.extend(batch.into_iter().map(|b| b.name));
		if !full {
			break;
		}
	}
	Ok(default_first(names, info.default_branch.as_deref()))
}

#[derive(Deserialize)]
struct Hook {
	id: i64,
}

pub(super) async fn create_hook(
	t: &Target<'_>,
	repo: &str,
	url: &str,
	secret: &str,
) -> Result<String, ServerError> {
	let repo = checked(repo)?;
	let r = with_json(
		req(t, reqwest::Method::POST, &format!("/repos/{repo}/hooks"))?,
		&json!({
			"type": "gitea",
			"active": true,
			"events": ["push"],
			"config": { "url": url, "content_type": "json", "secret": secret },
		}),
	);
	let hook: Hook =
		read_json(send(r, t.base_url, "create the webhook").await?, t.base_url).await?;
	Ok(hook.id.to_string())
}

pub(super) async fn delete_hook(t: &Target<'_>, repo: &str, hook: &str) -> Result<(), ServerError> {
	let repo = checked(repo)?;
	let Ok(hook) = hook.parse::<i64>() else {
		return Ok(());
	};
	let r = req(
		t,
		reqwest::Method::DELETE,
		&format!("/repos/{repo}/hooks/{hook}"),
	)?;
	send(r, t.base_url, "delete the webhook").await?;
	Ok(())
}
