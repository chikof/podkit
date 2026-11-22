//! What autodeploy asks a provider about a push: do the commit's checks pass, which files did
//! the push change, and a commit status to show where the deploy is.
//!
//! Each shape is the provider's documented one:
//! - GitHub: <https://docs.github.com/en/rest/checks/suites#list-check-suites-for-a-git-reference>,
//!   <https://docs.github.com/en/rest/commits/statuses#get-the-combined-status-for-a-specific-reference>,
//!   <https://docs.github.com/en/rest/commits/statuses#create-a-commit-status>,
//!   <https://docs.github.com/en/rest/commits/commits#compare-two-commits>. The app needs the
//!   `checks: read` and `statuses: write` permissions.
//! - GitLab: <https://docs.gitlab.com/api/pipelines/> (`GET /projects/:id/pipelines?sha=`),
//!   <https://docs.gitlab.com/api/commits/> (`POST /projects/:id/statuses/:sha`, commit diff),
//!   <https://docs.gitlab.com/api/repositories/#compare-branches-tags-or-commits>.
//! - Gitea: `GET /repos/{owner}/{repo}/commits/{ref}/status`, `POST /repos/{owner}/{repo}/statuses/{sha}`
//!   and `GET /repos/{owner}/{repo}/git/commits/{sha}` in its swagger
//!   (`routers/api/v1/repo/status.go`, `commits.go`).

use serde::Deserialize;
use serde_json::json;

use super::api::{
	STATUS_CONTEXT, StatusPost, StatusState, Target, checked_sha, dedup, is_zero_sha,
	json as read_json, send, short_description, with_json, with_query,
};
use super::{gitea, github, gitlab};
use crate::autodeploy::rules::{Check, Ci, combine};
use crate::error::ServerError;

/// A status word as GitHub and Gitea write them (Gitea adds `warning`, `running`, `skipped`).
fn commit_status_check(state: &str) -> Check {
	match state {
		"failure" | "error" => Check::Failed,
		"pending" | "running" => Check::Running,
		// success, warning, skipped
		_ => Check::Ok,
	}
}

fn state_word(state: StatusState, failure: &'static str, cancelled: &'static str) -> &'static str {
	match state {
		StatusState::Pending => "pending",
		StatusState::Success => "success",
		StatusState::Failure => failure,
		StatusState::Cancelled => cancelled,
	}
}

// ---- GitHub -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Suites {
	#[serde(default)]
	check_suites: Vec<Suite>,
}

#[derive(Deserialize)]
struct Suite {
	#[serde(default)]
	status: Option<String>,
	#[serde(default)]
	conclusion: Option<String>,
	#[serde(default)]
	latest_check_runs_count: i64,
	#[serde(default)]
	app: Option<SuiteApp>,
}

#[derive(Deserialize)]
struct SuiteApp {
	#[serde(default)]
	slug: Option<String>,
}

#[derive(Deserialize)]
struct Combined {
	#[serde(default)]
	statuses: Vec<StatusRow>,
}

#[derive(Deserialize)]
struct StatusRow {
	/// GitHub calls it `state`; Gitea's per-status field is `status`.
	#[serde(alias = "status", default)]
	state: String,
	#[serde(default)]
	context: String,
}

pub(super) async fn github_ci(t: &Target<'_>, repo: &str, sha: &str) -> Result<Ci, ServerError> {
	let sha = checked_sha(sha)?;
	let token = github::installation_token(t, repo).await?;
	let api = github::api_base(t.base_url);
	let get =
		|path: String| github::headers(t.http.get(format!("{api}{path}"))).bearer_auth(&token);

	let suites: Suites = read_json(
		send(
			get(format!(
				"/repos/{repo}/commits/{sha}/check-suites?per_page=100"
			)),
			t.base_url,
			"read the check suites",
		)
		.await
		.map_err(installation_error)?,
		t.base_url,
	)
	.await?;
	let mut checks: Vec<(String, Check)> = Vec::new();
	for suite in suites.check_suites {
		// GitHub makes an empty suite for every app that could run checks; it never finishes.
		if suite.latest_check_runs_count == 0 {
			continue;
		}
		let name = suite
			.app
			.and_then(|a| a.slug)
			.unwrap_or_else(|| "check suite".to_owned());
		let check = if suite.status.as_deref() == Some("completed") {
			match suite.conclusion.as_deref() {
				Some("success" | "neutral" | "skipped") => Check::Ok,
				_ => Check::Failed,
			}
		} else {
			Check::Running
		};
		checks.push((name, check));
	}

	let combined: Combined = read_json(
		send(
			get(format!("/repos/{repo}/commits/{sha}/status?per_page=100")),
			t.base_url,
			"read the commit statuses",
		)
		.await?,
		t.base_url,
	)
	.await?;
	checks.extend(
		combined
			.statuses
			.into_iter()
			.filter(|s| s.context != STATUS_CONTEXT)
			.map(|s| (s.context, commit_status_check(&s.state))),
	);
	Ok(combine(&checks))
}

#[derive(Deserialize)]
struct FileRow {
	#[serde(default)]
	filename: String,
	#[serde(default)]
	previous_filename: Option<String>,
}

#[derive(Deserialize)]
struct FileList {
	#[serde(default)]
	files: Vec<FileRow>,
}

/// Compare returns at most 300 files, 100 a page.
pub(super) async fn github_changed_files(
	t: &Target<'_>,
	repo: &str,
	before: Option<&str>,
	after: &str,
) -> Result<Vec<String>, ServerError> {
	let after = checked_sha(after)?;
	let token = github::installation_token(t, repo).await?;
	let api = github::api_base(t.base_url);
	let mut files = Vec::new();
	for page in 1..=3 {
		let path = match before.filter(|b| !is_zero_sha(b)) {
			Some(before) => format!(
				"/repos/{repo}/compare/{}...{after}?per_page=100&page={page}",
				checked_sha(before)?
			),
			None => format!("/repos/{repo}/commits/{after}?per_page=100&page={page}"),
		};
		let req = github::headers(t.http.get(format!("{api}{path}"))).bearer_auth(&token);
		let list: FileList = read_json(
			send(req, t.base_url, "read the changed files").await?,
			t.base_url,
		)
		.await?;
		let full = list.files.len() >= 100;
		for f in list.files {
			files.push(f.filename);
			files.extend(f.previous_filename);
		}
		if !full {
			break;
		}
	}
	Ok(dedup(files))
}

pub(super) async fn github_post_status(
	t: &Target<'_>,
	repo: &str,
	sha: &str,
	post: &StatusPost<'_>,
) -> Result<(), ServerError> {
	let sha = checked_sha(sha)?;
	let token = github::installation_token(t, repo).await?;
	let req = github::headers(t.http.post(format!(
		"{}/repos/{repo}/statuses/{sha}",
		github::api_base(t.base_url)
	)))
	.bearer_auth(token);
	let mut body = json!({
		"state": state_word(post.state, "failure", "error"),
		"description": short_description(post.description),
		"context": STATUS_CONTEXT,
	});
	if let Some(link) = post.link {
		body["target_url"] = link.into();
	}
	send(with_json(req, &body), t.base_url, "post the commit status")
		.await
		.map_err(installation_error)?;
	Ok(())
}

// ---- Gitea --------------------------------------------------------------------------------

pub(super) async fn gitea_ci(t: &Target<'_>, repo: &str, sha: &str) -> Result<Ci, ServerError> {
	let repo = gitea::checked(repo)?;
	let sha = checked_sha(sha)?;
	let req = gitea::req(
		t,
		reqwest::Method::GET,
		&with_query(
			&format!("/repos/{repo}/commits/{sha}/status"),
			&[("limit", "50")],
		),
	)?;
	let combined: Combined = read_json(
		send(req, t.base_url, "read the commit statuses").await?,
		t.base_url,
	)
	.await?;
	let checks: Vec<(String, Check)> = combined
		.statuses
		.into_iter()
		.filter(|s| s.context != STATUS_CONTEXT)
		.map(|s| (s.context, commit_status_check(&s.state)))
		.collect();
	Ok(combine(&checks))
}

#[derive(Deserialize)]
struct GiteaCommit {
	#[serde(default)]
	files: Vec<GiteaFile>,
}

#[derive(Deserialize)]
struct GiteaFile {
	#[serde(default)]
	filename: String,
}

/// Gitea's compare answers with commits, not files, so each commit of the push is read
/// (at most 20: the number a push webhook lists). A push with no commit list reads `after`.
pub(super) async fn gitea_changed_files(
	t: &Target<'_>,
	repo: &str,
	after: &str,
	commits: &[String],
) -> Result<Vec<String>, ServerError> {
	let repo = gitea::checked(repo)?;
	let mut ids: Vec<&str> = commits.iter().map(String::as_str).take(20).collect();
	if ids.is_empty() {
		ids.push(after);
	}
	let mut files = Vec::new();
	for id in ids {
		let id = checked_sha(id)?;
		let req = gitea::req(
			t,
			reqwest::Method::GET,
			&with_query(
				&format!("/repos/{repo}/git/commits/{id}"),
				&[
					("stat", "false"),
					("verification", "false"),
					("files", "true"),
				],
			),
		)?;
		let commit: GiteaCommit =
			read_json(send(req, t.base_url, "read the commit").await?, t.base_url).await?;
		files.extend(commit.files.into_iter().map(|f| f.filename));
	}
	Ok(dedup(files))
}

pub(super) async fn gitea_post_status(
	t: &Target<'_>,
	repo: &str,
	sha: &str,
	post: &StatusPost<'_>,
) -> Result<(), ServerError> {
	let repo = gitea::checked(repo)?;
	let sha = checked_sha(sha)?;
	let req = gitea::req(
		t,
		reqwest::Method::POST,
		&format!("/repos/{repo}/statuses/{sha}"),
	)?;
	let mut body = json!({
		"state": state_word(post.state, "failure", "error"),
		"description": short_description(post.description),
		"context": STATUS_CONTEXT,
	});
	if let Some(link) = post.link {
		body["target_url"] = link.into();
	}
	send(with_json(req, &body), t.base_url, "post the commit status").await?;
	Ok(())
}

// ---- GitLab -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Pipeline {
	id: i64,
	#[serde(default)]
	status: String,
	#[serde(default)]
	source: Option<String>,
}

pub(super) async fn gitlab_ci(t: &Target<'_>, repo: &str, sha: &str) -> Result<Ci, ServerError> {
	let id = gitlab::project(repo)?;
	let sha = checked_sha(sha)?;
	let req = gitlab::req(
		t,
		reqwest::Method::GET,
		&with_query(
			&format!("/projects/{id}/pipelines"),
			&[("sha", sha), ("per_page", "20")],
		),
	)?;
	let pipelines: Vec<Pipeline> = read_json(
		send(req, t.base_url, "read the pipelines").await?,
		t.base_url,
	)
	.await?;
	// A commit status posted through the API (podkit's own) shows up as an `external`
	// pipeline; it is not CI. Of the rest, the newest one counts: an earlier one was retried.
	let latest = pipelines
		.into_iter()
		.filter(|p| p.source.as_deref() != Some("external"))
		.max_by_key(|p| p.id);
	let Some(p) = latest else {
		return Ok(Ci::NoChecks);
	};
	let check = match p.status.as_str() {
		"success" | "skipped" | "manual" => Check::Ok,
		"failed" | "canceled" => Check::Failed,
		// created, waiting_for_resource, preparing, pending, running, scheduled
		_ => Check::Running,
	};
	Ok(combine(&[(format!("pipeline #{}", p.id), check)]))
}

#[derive(Deserialize)]
struct GitlabDiff {
	#[serde(default)]
	old_path: String,
	#[serde(default)]
	new_path: String,
}

#[derive(Deserialize)]
struct GitlabCompare {
	#[serde(default)]
	diffs: Vec<GitlabDiff>,
}

pub(super) async fn gitlab_changed_files(
	t: &Target<'_>,
	repo: &str,
	before: Option<&str>,
	after: &str,
) -> Result<Vec<String>, ServerError> {
	let id = gitlab::project(repo)?;
	let after = checked_sha(after)?;
	let diffs: Vec<GitlabDiff> = if let Some(before) = before.filter(|b| !is_zero_sha(b)) {
		let req = gitlab::req(
			t,
			reqwest::Method::GET,
			&with_query(
				&format!("/projects/{id}/repository/compare"),
				&[
					("from", checked_sha(before)?),
					("to", after),
					("straight", "true"),
				],
			),
		)?;
		let c: GitlabCompare = read_json(
			send(req, t.base_url, "compare the commits").await?,
			t.base_url,
		)
		.await?;
		c.diffs
	} else {
		let req = gitlab::req(
			t,
			reqwest::Method::GET,
			&with_query(
				&format!("/projects/{id}/repository/commits/{after}/diff"),
				&[("per_page", "100")],
			),
		)?;
		read_json(send(req, t.base_url, "read the commit").await?, t.base_url).await?
	};
	Ok(dedup(
		diffs.into_iter().flat_map(|d| [d.new_path, d.old_path]),
	))
}

pub(super) async fn gitlab_post_status(
	t: &Target<'_>,
	repo: &str,
	sha: &str,
	post: &StatusPost<'_>,
) -> Result<(), ServerError> {
	let id = gitlab::project(repo)?;
	let sha = checked_sha(sha)?;
	let description = short_description(post.description);
	let mut params = vec![
		("state", state_word(post.state, "failed", "canceled")),
		("name", STATUS_CONTEXT),
		("description", description.as_str()),
	];
	if let Some(link) = post.link {
		params.push(("target_url", link));
	}
	let req = gitlab::req(
		t,
		reqwest::Method::POST,
		&with_query(&format!("/projects/{id}/statuses/{sha}"), &params),
	)?;
	send(req, t.base_url, "post the commit status").await?;
	Ok(())
}

/// A refusal from GitHub on a checks or statuses call usually means the app was made before
/// podkit asked for those permissions; say so.
fn installation_error(e: ServerError) -> ServerError {
	match e {
		ServerError::GitProvider(m) if m.contains("HTTP 403") || m.contains("answered 404") => {
			ServerError::GitProvider(format!(
				"{m} The GitHub App needs the Checks (read) and Commit statuses (read and write) permissions: \
				 open its settings on GitHub, grant them and accept the update on the installation."
			))
		}
		other => other,
	}
}
