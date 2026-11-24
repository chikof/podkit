//! What previews ask a provider about pull requests: the pull request itself, who may approve
//! a preview, and the one comment podkit keeps up to date in the pull request.
//!
//! Each shape is the provider's documented one:
//! - GitHub: <https://docs.github.com/en/rest/pulls/pulls#get-a-pull-request>,
//!   <https://docs.github.com/en/rest/issues/comments#create-an-issue-comment> and
//!   `#update-an-issue-comment` (a pull request is an issue for comments),
//!   <https://docs.github.com/en/rest/collaborators/collaborators#get-repository-permissions-for-a-user>.
//!   The app needs `Pull requests: read` and `Issues: write` (comments).
//! - GitLab: <https://docs.gitlab.com/api/merge_requests/#get-single-mr>,
//!   <https://docs.gitlab.com/api/notes/#create-new-merge-request-note> and `#modify-existing-merge-request-note`,
//!   <https://docs.gitlab.com/api/members/#get-a-member-of-a-group-or-project-including-inherited-and-invited-members>.
//! - Gitea: `GET /repos/{owner}/{repo}/pulls/{index}`, `POST /repos/{owner}/{repo}/issues/{index}/comments`,
//!   `PATCH /repos/{owner}/{repo}/issues/comments/{id}`,
//!   `GET /repos/{owner}/{repo}/collaborators/{collaborator}/permission` in its swagger
//!   (`modules/structs/pull.go`, `issue_comment.go`, `repo_collaborator.go`).

use serde::Deserialize;
use serde_json::json;

use super::api::{Target, json as read_json, send, with_json};
use super::{Kind, gitea, github, gitlab};
use crate::error::ServerError;

/// A pull request (a merge request on GitLab) as the previews need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
	pub number: i64,
	pub title: String,
	pub open: bool,
	pub merged: bool,
	pub head_sha: String,
	pub head_ref: String,
	/// `owner/name` of the repository the head branch is in; empty when it was deleted.
	pub head_repo: String,
	pub base_ref: String,
	pub base_repo: String,
}

/// The longest comment body podkit writes.
const MAX_BODY: usize = 4000;

#[derive(Deserialize)]
struct Branch {
	#[serde(default, rename = "ref")]
	name: String,
	#[serde(default)]
	sha: String,
	#[serde(default)]
	repo: Option<RepoName>,
}

#[derive(Deserialize)]
struct RepoName {
	#[serde(default)]
	full_name: String,
}

/// GitHub's and Gitea's pull request: they agree on everything read here.
#[derive(Deserialize)]
struct Pull {
	number: i64,
	#[serde(default)]
	title: String,
	#[serde(default)]
	state: String,
	#[serde(default)]
	merged: bool,
	head: Branch,
	base: Branch,
}

impl Pull {
	fn into_request(self) -> PullRequest {
		PullRequest {
			number: self.number,
			title: self.title,
			open: self.state == "open",
			merged: self.merged,
			head_sha: self.head.sha,
			head_ref: self.head.name,
			head_repo: self.head.repo.map(|r| r.full_name).unwrap_or_default(),
			base_ref: self.base.name,
			base_repo: self.base.repo.map(|r| r.full_name).unwrap_or_default(),
		}
	}
}

#[derive(Deserialize)]
struct Merge {
	iid: i64,
	#[serde(default)]
	title: String,
	#[serde(default)]
	state: String,
	#[serde(default)]
	source_branch: String,
	#[serde(default)]
	target_branch: String,
	#[serde(default)]
	sha: String,
	#[serde(default)]
	source_project_id: i64,
}

#[derive(Deserialize)]
struct GitlabProject {
	#[serde(default)]
	path_with_namespace: String,
}

#[derive(Deserialize)]
struct Made {
	id: i64,
}

#[derive(Deserialize)]
struct Permission {
	#[serde(default)]
	permission: String,
	#[serde(default)]
	role_name: String,
}

#[derive(Deserialize)]
struct Member {
	#[serde(default)]
	access_level: i64,
}

fn body_of(text: &str) -> String {
	text.chars().take(MAX_BODY).collect()
}

fn checked_login(login: &str) -> Result<&str, ServerError> {
	let ok = !login.is_empty()
		&& login.len() <= 100
		&& login
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'[' | b']'));
	if ok {
		Ok(login)
	} else {
		Err(ServerError::Validation(
			"That user name is not valid.".to_owned(),
		))
	}
}

impl Target<'_> {
	/// Pull request `number` of `repo`.
	pub async fn pull_request(&self, repo: &str, number: i64) -> Result<PullRequest, ServerError> {
		match self.kind {
			Kind::GithubApp => {
				let token = github::installation_token(self, repo).await?;
				let url = format!(
					"{}/repos/{repo}/pulls/{number}",
					github::api_base(self.base_url)
				);
				let pull: Pull = read_json(
					send(
						github::headers(self.http.get(url)).bearer_auth(token),
						self.base_url,
						"read the pull request",
					)
					.await?,
					self.base_url,
				)
				.await?;
				Ok(pull.into_request())
			}
			Kind::Gitea => {
				let repo = gitea::checked(repo)?;
				let req = gitea::req(
					self,
					reqwest::Method::GET,
					&format!("/repos/{repo}/pulls/{number}"),
				)?;
				let pull: Pull = read_json(
					send(req, self.base_url, "read the pull request").await?,
					self.base_url,
				)
				.await?;
				Ok(pull.into_request())
			}
			Kind::Gitlab => {
				let id = gitlab::project(repo)?;
				let req = gitlab::req(
					self,
					reqwest::Method::GET,
					&format!("/projects/{id}/merge_requests/{number}"),
				)?;
				let mr: Merge = read_json(
					send(req, self.base_url, "read the merge request").await?,
					self.base_url,
				)
				.await?;
				let head_repo = if mr.source_project_id > 0 {
					let req = gitlab::req(
						self,
						reqwest::Method::GET,
						&format!("/projects/{}", mr.source_project_id),
					)?;
					let project: GitlabProject = read_json(
						send(req, self.base_url, "read the source project").await?,
						self.base_url,
					)
					.await?;
					project.path_with_namespace
				} else {
					String::new()
				};
				Ok(PullRequest {
					number: mr.iid,
					title: mr.title,
					open: mr.state == "opened",
					merged: mr.state == "merged",
					head_sha: mr.sha,
					head_ref: mr.source_branch,
					head_repo,
					base_ref: mr.target_branch,
					base_repo: repo.to_owned(),
				})
			}
		}
	}

	/// Whether `login` may write to `repo`: a maintainer who can approve a preview of a fork.
	/// GitLab asks by the user's id, which its webhook carries.
	pub async fn can_write(
		&self,
		repo: &str,
		login: &str,
		user_id: Option<i64>,
	) -> Result<bool, ServerError> {
		match self.kind {
			Kind::GithubApp => {
				let login = checked_login(login)?;
				let token = github::installation_token(self, repo).await?;
				let url = format!(
					"{}/repos/{repo}/collaborators/{login}/permission",
					github::api_base(self.base_url)
				);
				let p: Permission = read_json(
					send(
						github::headers(self.http.get(url)).bearer_auth(token),
						self.base_url,
						"read the commenter's permission",
					)
					.await?,
					self.base_url,
				)
				.await?;
				Ok(writes(&p))
			}
			Kind::Gitea => {
				let repo = gitea::checked(repo)?;
				let login = checked_login(login)?;
				let req = gitea::req(
					self,
					reqwest::Method::GET,
					&format!("/repos/{repo}/collaborators/{login}/permission"),
				)?;
				let p: Permission = read_json(
					send(req, self.base_url, "read the commenter's permission").await?,
					self.base_url,
				)
				.await?;
				Ok(writes(&p))
			}
			Kind::Gitlab => {
				let Some(user) = user_id.filter(|u| *u > 0) else {
					return Ok(false);
				};
				let id = gitlab::project(repo)?;
				let req = gitlab::req(
					self,
					reqwest::Method::GET,
					&format!("/projects/{id}/members/all/{user}"),
				)?;
				match send(req, self.base_url, "read the commenter's access").await {
					Ok(res) => {
						let m: Member = read_json(res, self.base_url).await?;
						// 30 is Developer, who may push to the project.
						Ok(m.access_level >= 30)
					}
					// Not a member at all.
					Err(ServerError::GitProvider(m)) if m.contains("answered 404") => Ok(false),
					Err(e) => Err(e),
				}
			}
		}
	}

	/// Writes `body` as podkit's comment in pull request `number`: edits comment `existing`
	/// when there is one and it is still there, otherwise makes a new one. Returns the
	/// comment's id.
	pub async fn upsert_comment(
		&self,
		repo: &str,
		number: i64,
		existing: Option<&str>,
		body: &str,
	) -> Result<String, ServerError> {
		let body = body_of(body);
		if let Some(id) =
			existing.filter(|i| i.bytes().all(|b| b.is_ascii_digit()) && !i.is_empty())
		{
			match self.edit_comment(repo, number, id, &body).await {
				Ok(()) => return Ok(id.to_owned()),
				// Someone deleted it: write a new one.
				Err(ServerError::GitProvider(m)) if m.contains("answered 404") => {}
				Err(e) => return Err(e),
			}
		}
		self.make_comment(repo, number, &body).await
	}

	async fn make_comment(
		&self,
		repo: &str,
		number: i64,
		body: &str,
	) -> Result<String, ServerError> {
		let payload = json!({ "body": body });
		let made: Made = match self.kind {
			Kind::GithubApp => {
				let token = github::installation_token(self, repo).await?;
				let url = format!(
					"{}/repos/{repo}/issues/{number}/comments",
					github::api_base(self.base_url)
				);
				let req = github::headers(self.http.post(url)).bearer_auth(token);
				read_json(
					send(
						with_json(req, &payload),
						self.base_url,
						"comment on the pull request",
					)
					.await?,
					self.base_url,
				)
				.await?
			}
			Kind::Gitea => {
				let repo = gitea::checked(repo)?;
				let req = gitea::req(
					self,
					reqwest::Method::POST,
					&format!("/repos/{repo}/issues/{number}/comments"),
				)?;
				read_json(
					send(
						with_json(req, &payload),
						self.base_url,
						"comment on the pull request",
					)
					.await?,
					self.base_url,
				)
				.await?
			}
			Kind::Gitlab => {
				let id = gitlab::project(repo)?;
				let req = gitlab::req(
					self,
					reqwest::Method::POST,
					&format!("/projects/{id}/merge_requests/{number}/notes"),
				)?;
				read_json(
					send(
						with_json(req, &payload),
						self.base_url,
						"comment on the merge request",
					)
					.await?,
					self.base_url,
				)
				.await?
			}
		};
		Ok(made.id.to_string())
	}

	async fn edit_comment(
		&self,
		repo: &str,
		number: i64,
		id: &str,
		body: &str,
	) -> Result<(), ServerError> {
		let payload = json!({ "body": body });
		match self.kind {
			Kind::GithubApp => {
				let token = github::installation_token(self, repo).await?;
				let url = format!(
					"{}/repos/{repo}/issues/comments/{id}",
					github::api_base(self.base_url)
				);
				let req = github::headers(self.http.patch(url)).bearer_auth(token);
				send(with_json(req, &payload), self.base_url, "edit the comment").await?;
			}
			Kind::Gitea => {
				let repo = gitea::checked(repo)?;
				let req = gitea::req(
					self,
					reqwest::Method::PATCH,
					&format!("/repos/{repo}/issues/comments/{id}"),
				)?;
				send(with_json(req, &payload), self.base_url, "edit the comment").await?;
			}
			Kind::Gitlab => {
				let project = gitlab::project(repo)?;
				let req = gitlab::req(
					self,
					reqwest::Method::PUT,
					&format!("/projects/{project}/merge_requests/{number}/notes/{id}"),
				)?;
				send(with_json(req, &payload), self.base_url, "edit the comment").await?;
			}
		}
		Ok(())
	}
}

/// Whether a permission answer lets the user push: `admin`, `write` or `maintain` (GitHub's
/// `permission` is the legacy four, its `role_name` the real role; Gitea says `owner` for the
/// owner).
fn writes(p: &Permission) -> bool {
	["admin", "owner", "write", "maintain"]
		.iter()
		.any(|w| p.permission == *w || p.role_name == *w)
}
