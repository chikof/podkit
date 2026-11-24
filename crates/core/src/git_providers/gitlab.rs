//! GitLab client: an access token (`read_repository` to clone, `api` to make webhooks and
//! to list projects) against `{base}/api/v4`.

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::json;

use super::Credentials;
use super::api::{
	Identity, PAGE_SIZE, Repo, RepoPage, Target, json as read_json, segment, send, valid_repo,
	with_json, with_query, wrong_credentials,
};
use super::github::default_first;
use crate::error::ServerError;

fn token<'a>(t: &'a Target<'_>) -> Result<&'a str, ServerError> {
	match t.credentials {
		Credentials::Token { token } => Ok(token),
		Credentials::GithubApp { .. } => Err(wrong_credentials()),
	}
}

pub(super) fn req(
	t: &Target<'_>,
	method: reqwest::Method,
	path: &str,
) -> Result<RequestBuilder, ServerError> {
	Ok(t.http
		.request(method, format!("{}/api/v4{path}", t.base_url))
		.header("private-token", token(t)?)
		.header("user-agent", "podkit"))
}

pub(super) fn project(repo: &str) -> Result<String, ServerError> {
	if !valid_repo(repo) {
		return Err(ServerError::Validation(
			"That repository name is not valid.".to_owned(),
		));
	}
	Ok(segment(repo))
}

#[derive(Deserialize)]
struct User {
	username: String,
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
		detail: format!("Signed in as {}.", user.username),
		login: user.username,
	})
}

#[derive(Deserialize)]
struct Project {
	path_with_namespace: String,
	#[serde(default)]
	default_branch: Option<String>,
	#[serde(default)]
	visibility: Option<String>,
}

pub(super) async fn repos(t: &Target<'_>, query: &str, page: u32) -> Result<RepoPage, ServerError> {
	let (per_page, page) = (PAGE_SIZE.to_string(), page.to_string());
	let mut params = vec![
		("membership", "true"),
		("simple", "true"),
		("order_by", "last_activity_at"),
		("per_page", per_page.as_str()),
		("page", page.as_str()),
	];
	if !query.trim().is_empty() {
		params.extend([("search", query.trim()), ("search_namespaces", "true")]);
	}
	let r = req(t, reqwest::Method::GET, &with_query("/projects", &params))?;
	let res = send(r, t.base_url, "list the projects").await?;
	// GitLab names the next page in a header, empty on the last one.
	let next_page = res
		.headers()
		.get("x-next-page")
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.parse::<u32>().ok());
	let rows: Vec<Project> = read_json(res, t.base_url).await?;
	Ok(RepoPage {
		repos: rows
			.into_iter()
			.map(|p| Repo {
				full_name: p.path_with_namespace,
				default_branch: p.default_branch.unwrap_or_else(|| "main".to_owned()),
				private: p.visibility.as_deref() != Some("public"),
			})
			.collect(),
		next_page,
	})
}

#[derive(Deserialize)]
struct Branch {
	name: String,
	#[serde(default)]
	default: bool,
}

pub(super) async fn branches(t: &Target<'_>, repo: &str) -> Result<Vec<String>, ServerError> {
	let id = project(repo)?;
	let mut names = Vec::new();
	let mut default = None;
	for page in 1..=10 {
		let r = req(
			t,
			reqwest::Method::GET,
			&with_query(
				&format!("/projects/{id}/repository/branches"),
				&[("per_page", "100"), ("page", &page.to_string())],
			),
		)?;
		let batch: Vec<Branch> =
			read_json(send(r, t.base_url, "list the branches").await?, t.base_url).await?;
		let full = batch.len() == 100;
		for b in batch {
			if b.default {
				default = Some(b.name.clone());
			}
			names.push(b.name);
		}
		if !full {
			break;
		}
	}
	Ok(default_first(names, default.as_deref()))
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
	let id = project(repo)?;
	let r = with_json(
		req(t, reqwest::Method::POST, &format!("/projects/{id}/hooks"))?,
		&json!({
			"url": url,
			"token": secret,
			"push_events": true,
			"merge_requests_events": true,
			"note_events": true,
			"enable_ssl_verification": url.starts_with("https://"),
		}),
	);
	let hook: Hook =
		read_json(send(r, t.base_url, "create the webhook").await?, t.base_url).await?;
	Ok(hook.id.to_string())
}

pub(super) async fn delete_hook(t: &Target<'_>, repo: &str, hook: &str) -> Result<(), ServerError> {
	let id = project(repo)?;
	let Ok(hook) = hook.parse::<i64>() else {
		return Ok(());
	};
	let r = req(
		t,
		reqwest::Method::DELETE,
		&format!("/projects/{id}/hooks/{hook}"),
	)?;
	send(r, t.base_url, "delete the webhook").await?;
	Ok(())
}
