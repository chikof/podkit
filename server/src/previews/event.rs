//! Pull request events as the three providers send them, read into one shape.
//!
//! The payloads are the providers' documented ones:
//! - GitHub: <https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request>
//!   (`action` opened, reopened, synchronize, closed; `pull_request.merged`) and
//!   `#issue_comment` (`action` created; `issue.pull_request` is set for a pull request).
//! - GitLab: <https://docs.gitlab.com/user/project/integrations/webhook_events/#merge-request-events>
//!   (`object_attributes.action` open, reopen, update, close, merge; `oldrev` is set when
//!   the update brought commits) and `#comment-events` (`noteable_type` `MergeRequest`).
//! - Gitea: `modules/structs/hook.go` (`PullRequestPayload` with `action` opened, reopened,
//!   synchronized, closed, and `IssueCommentPayload` with `is_pull`); the header
//!   `X-Gitea-Event` is `pull_request` or `issue_comment`.

use serde_json::Value;

use crate::git_providers::Kind;

/// The pull request an event is about, with what a preview is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
	/// `owner/name` the pull request was opened against.
	pub repo: String,
	pub number: i64,
	pub title: String,
	pub head_sha: String,
	pub head_ref: String,
	/// `owner/name` of the head branch's repository; empty when that repository is gone.
	pub head_repo: String,
	pub base_ref: String,
}

impl Pr {
	/// Whether the head branch lives in another repository than the one the pull request
	/// is against (or in none that is known): its code is not the team's.
	#[must_use]
	pub fn is_fork(&self) -> bool {
		self.head_repo.is_empty() || !self.head_repo.eq_ignore_ascii_case(&self.repo)
	}
}

/// What happened, as far as previews care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
	/// Opened or reopened: a preview is wanted.
	Opened(Pr),
	/// The head branch got commits.
	Synced(Pr),
	/// Closed without merging, or merged: the preview goes.
	Closed { pr: Pr, merged: bool },
	/// A comment on a pull request.
	Comment {
		repo: String,
		number: i64,
		body: String,
		login: String,
		user_id: Option<i64>,
	},
}

fn text(v: &Value, pointer: &str) -> String {
	v.pointer(pointer)
		.and_then(Value::as_str)
		.unwrap_or_default()
		.to_owned()
}

fn int(v: &Value, pointer: &str) -> Option<i64> {
	v.pointer(pointer).and_then(Value::as_i64)
}

fn flag(v: &Value, pointer: &str) -> bool {
	v.pointer(pointer).and_then(Value::as_bool).unwrap_or(false)
}

/// The pull request of a GitHub or Gitea payload, which share their shape.
fn hub_pr(v: &Value) -> Option<Pr> {
	let number = int(v, "/pull_request/number").or_else(|| int(v, "/number"))?;
	Some(Pr {
		repo: text(v, "/repository/full_name"),
		number,
		title: text(v, "/pull_request/title"),
		head_sha: text(v, "/pull_request/head/sha"),
		head_ref: text(v, "/pull_request/head/ref"),
		head_repo: text(v, "/pull_request/head/repo/full_name"),
		base_ref: text(v, "/pull_request/base/ref"),
	})
	.filter(|pr| !pr.repo.is_empty())
}

fn hub_comment(v: &Value, is_pull: bool) -> Option<Event> {
	if text(v, "/action") != "created" || !is_pull {
		return None;
	}
	Some(Event::Comment {
		repo: text(v, "/repository/full_name"),
		number: int(v, "/issue/number")?,
		body: text(v, "/comment/body"),
		login: {
			let login = text(v, "/comment/user/login");
			if login.is_empty() {
				text(v, "/comment/user/username")
			} else {
				login
			}
		},
		user_id: int(v, "/comment/user/id"),
	})
}

/// `event` is the `X-GitHub-Event` header.
fn github(event: &str, v: &Value) -> Option<Event> {
	match event {
		"pull_request" => {
			let pr = hub_pr(v)?;
			match text(v, "/action").as_str() {
				"opened" | "reopened" => Some(Event::Opened(pr)),
				"synchronize" => Some(Event::Synced(pr)),
				"closed" => Some(Event::Closed {
					merged: flag(v, "/pull_request/merged"),
					pr,
				}),
				_ => None,
			}
		}
		"issue_comment" => hub_comment(v, v.pointer("/issue/pull_request").is_some()),
		_ => None,
	}
}

/// `event` is the `X-Gitea-Event` header.
fn gitea(event: &str, v: &Value) -> Option<Event> {
	match event {
		"pull_request" => {
			let pr = hub_pr(v)?;
			match text(v, "/action").as_str() {
				"opened" | "reopened" => Some(Event::Opened(pr)),
				"synchronized" => Some(Event::Synced(pr)),
				"closed" => Some(Event::Closed {
					merged: flag(v, "/pull_request/merged"),
					pr,
				}),
				_ => None,
			}
		}
		"issue_comment" => hub_comment(
			v,
			flag(v, "/is_pull")
				|| v.pointer("/issue/pull_request")
					.is_some_and(|p| !p.is_null()),
		),
		_ => None,
	}
}

/// `event` is the `X-Gitlab-Event` header.
fn gitlab(event: &str, v: &Value) -> Option<Event> {
	match event {
		"Merge Request Hook" => {
			let a = "/object_attributes";
			let repo = {
				let target = text(v, &format!("{a}/target/path_with_namespace"));
				if target.is_empty() {
					text(v, "/project/path_with_namespace")
				} else {
					target
				}
			};
			if repo.is_empty() {
				return None;
			}
			let pr = Pr {
				repo,
				number: int(v, &format!("{a}/iid"))?,
				title: text(v, &format!("{a}/title")),
				head_sha: text(v, &format!("{a}/last_commit/id")),
				head_ref: text(v, &format!("{a}/source_branch")),
				head_repo: text(v, &format!("{a}/source/path_with_namespace")),
				base_ref: text(v, &format!("{a}/target_branch")),
			};
			match text(v, &format!("{a}/action")).as_str() {
				"open" | "reopen" => Some(Event::Opened(pr)),
				// Other updates (title, labels) carry no `oldrev`.
				"update" if !text(v, &format!("{a}/oldrev")).is_empty() => Some(Event::Synced(pr)),
				"close" => Some(Event::Closed { pr, merged: false }),
				"merge" => Some(Event::Closed { pr, merged: true }),
				_ => None,
			}
		}
		"Note Hook" => {
			if text(v, "/object_attributes/noteable_type") != "MergeRequest" {
				return None;
			}
			let action = text(v, "/object_attributes/action");
			if !action.is_empty() && action != "create" {
				return None;
			}
			Some(Event::Comment {
				repo: text(v, "/project/path_with_namespace"),
				number: int(v, "/merge_request/iid")?,
				body: text(v, "/object_attributes/note"),
				login: text(v, "/user/username"),
				user_id: int(v, "/user/id"),
			})
		}
		_ => None,
	}
}

/// Reads a webhook delivery: `kind` is who sent it, `event` the header that names the event
/// (`X-GitHub-Event`, `X-Gitea-Event`, `X-Gitlab-Event`). `None` for an event previews do not
/// follow and for a payload that is not what the event says.
#[must_use]
pub fn parse(kind: Kind, event: &str, body: &[u8]) -> Option<Event> {
	let v: Value = serde_json::from_slice(body).ok()?;
	match kind {
		Kind::GithubApp => github(event, &v),
		Kind::Gitea => gitea(event, &v),
		Kind::Gitlab => gitlab(event, &v),
	}
}

/// Whether a comment asks for a preview: some line of it is just `/preview`.
#[must_use]
pub fn asks_for_preview(body: &str) -> bool {
	body.lines()
		.map(str::trim)
		.any(|line| line.eq_ignore_ascii_case("/preview"))
}
