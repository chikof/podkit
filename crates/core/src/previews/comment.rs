//! What a preview says in its pull request: one comment, edited in place as the preview
//! moves on, and a commit status with the same state and the link.

use super::{Preview, State};
use crate::applications::Application;
use crate::git_providers::api::{PREVIEW_CONTEXT, StatusPost, StatusState};
use crate::git_providers::service as providers;
use crate::state::AppState;

/// The most of a failure's reason that reaches the pull request.
const REASON_MAX: usize = 300;

fn clip(text: &str, max: usize) -> String {
	let line = text.lines().next().unwrap_or_default().trim();
	let cut: String = line.chars().take(max).collect();
	if line.chars().count() > max {
		format!("{cut}…")
	} else {
		cut
	}
}

/// The comment's text for the preview as it is now.
#[must_use]
pub fn body(preview: &Preview, limit: i32, ttl_days: i32) -> String {
	let short: String = preview.head_sha.chars().take(7).collect();
	let head = "### podkit preview\n\n";
	let state = match preview.state() {
		State::AwaitingApproval if preview.environment_id.is_some() => {
			let running: String = preview.approved_sha.chars().take(7).collect();
			format!(
				"**New commits need a new `/preview`.** The branch moved on to `{short}` after this preview was approved, and podkit builds nothing it has not been shown. The preview keeps serving the approved commit `{running}` until someone with write access to the repository comments `/preview`."
			)
		}
		State::AwaitingApproval => "**Waiting for approval.** This pull request comes from a fork, so podkit builds nothing from it until someone with write access to the repository comments `/preview`.".to_owned(),
		State::Limited => format!(
			"**Waiting for a free place.** {limit} previews of this application are open, which is its limit. This one starts when another closes, or when someone comments `/preview` after making room."
		),
		State::Queued | State::Building => format!("**Building** `{short}`…"),
		State::Ready => format!(
			"**Ready:** {}\n\nCommit `{short}`, with a new empty database that nothing of production touches. It is removed when this pull request closes, or after {ttl_days} days without a push.",
			preview.url
		),
		State::Failed => format!(
			"**Failed** on `{short}`: {}\n\nPush again, or comment `/preview`, to try again.",
			clip(&preview.detail, REASON_MAX)
		),
		State::Removing => "**Removed.** The preview and its database are gone.".to_owned(),
	};
	format!("{head}{state}\n")
}

/// The status the preview's state is posted as, and its one-line description.
#[must_use]
pub fn status(preview: &Preview) -> (StatusState, String) {
	match preview.state() {
		State::AwaitingApproval if preview.environment_id.is_some() => (
			StatusState::Pending,
			"New commits need a new /preview".to_owned(),
		),
		State::AwaitingApproval => (
			StatusState::Pending,
			"Waiting for a maintainer's /preview".to_owned(),
		),
		State::Limited => (StatusState::Pending, "Waiting for a free place".to_owned()),
		State::Queued | State::Building => {
			(StatusState::Pending, "Building the preview".to_owned())
		}
		State::Ready => (StatusState::Success, "The preview is ready".to_owned()),
		State::Failed => (
			StatusState::Failure,
			format!("Preview failed: {}", clip(&preview.detail, 100)),
		),
		State::Removing => (StatusState::Cancelled, "The preview was removed".to_owned()),
	}
}

/// Writes the comment (making it the first time, editing it after) and the commit status.
/// Never fails: a provider that cannot be reached, or an app that lacks the permission, is a
/// line in the log, and the preview goes on.
pub async fn publish(s: &AppState, preview: &Preview, app: &Application, limit: i32, ttl: i32) {
	let (Some(provider), Some(repo)) = (app.git_provider_id, app.git_repo.as_deref()) else {
		return;
	};
	let text = body(preview, limit, ttl);
	match providers::upsert_comment(
		s,
		provider,
		repo,
		i64::from(preview.number),
		preview.comment_id.as_deref(),
		&text,
	)
	.await
	{
		Ok(id) => {
			if preview.comment_id.as_deref() != Some(id.as_str())
				&& let Err(e) = super::set_comment(&s.db, preview.id, &id).await
			{
				tracing::warn!(preview = preview.id.0, "cannot remember the comment: {e}");
			}
		}
		Err(e) => tracing::warn!(
			preview = preview.id.0,
			error = %e,
			"cannot write the preview's comment"
		),
	}
	if preview.head_sha.is_empty() || preview.state() == State::Removing {
		return;
	}
	let (state, description) = status(preview);
	let post = StatusPost {
		context: PREVIEW_CONTEXT,
		state,
		description: &description,
		link: (preview.state() == State::Ready).then_some(preview.url.as_str()),
	};
	if let Err(e) = providers::post_status(s, provider, repo, &preview.head_sha, &post).await {
		tracing::warn!(preview = preview.id.0, error = %e, "cannot post the preview's status");
	}
}
