//! What happens to a push once it has reached podkit: the rules decide whether it deploys,
//! the queue holds it while the commit's checks run or a freeze is open, and a release
//! replaces whatever an older push left waiting.
//!
//! The queue is a table, and its worker exists only while something is waiting: [`wake`]
//! starts one pass-and-sleep loop and the loop ends when the queue is empty. [`process`] is
//! the pass, with the clock as an argument.

use std::sync::atomic::AtomicBool;
#[cfg(not(test))]
use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use serde_json::Value;
use time::{Duration, OffsetDateTime};

use super::rules::{self, Ci};
use super::{NewPush, PushRow, Settings, State};
use crate::applications::{Application, SourceKind};
use crate::deployments::{self, Deployment};
use crate::error::AppResult;
use crate::git_providers::api::{StatusPost, StatusState, valid_sha};
use crate::git_providers::{self, branch_of};
use crate::ids::{ApplicationId, TeamId, UserId};
use crate::jobs::{self, Job, JobFailure, JobKind};
use crate::notifications::{self, Event, EventKind};
use crate::state::AppState;

/// How often the pass looks at what is waiting.
#[cfg_attr(test, allow(dead_code))]
const POLL: StdDuration = StdDuration::from_secs(15);
/// With checks asked for and none reported, how long to wait for the first to appear before
/// taking the commit as one nothing checks.
const NO_CHECKS_GRACE: Duration = Duration::seconds(90);
/// Finished pushes kept per application.
const KEEP: i64 = 30;

#[cfg_attr(test, allow(dead_code))]
static RUNNING: AtomicBool = AtomicBool::new(false);

/// A push, as the three providers' payloads describe it.
#[derive(Debug, Clone, Default)]
pub struct Push {
	/// `refs/heads/main`, `refs/tags/v1`; empty when the sender gave none.
	pub git_ref: String,
	pub before: Option<String>,
	pub after: Option<String>,
	/// The message of the newest commit.
	pub message: String,
	/// The ids of the commits the push brought.
	pub commits: Vec<String>,
	/// The files the payload itself lists, for a push the provider API cannot be asked about.
	pub payload_files: Option<Vec<String>>,
}

fn text(v: &Value, key: &str) -> Option<String> {
	v.get(key)
		.and_then(Value::as_str)
		.filter(|s| !s.is_empty())
		.map(str::to_owned)
}

impl Push {
	/// Reads GitHub's, Gitea's and GitLab's push payloads, which agree on `ref`, `before`,
	/// `after` and `commits[]` (`id`, `message`, `added`, `modified`, `removed`). What is
	/// missing stays empty.
	#[must_use]
	pub fn from_payload(body: &[u8]) -> Self {
		let Ok(v) = serde_json::from_slice::<Value>(body) else {
			return Self::default();
		};
		let commits: Vec<&Value> = v
			.get("commits")
			.and_then(Value::as_array)
			.map(|a| a.iter().collect())
			.unwrap_or_default();
		let after = text(&v, "after").or_else(|| text(&v, "checkout_sha"));
		let message = v
			.get("head_commit")
			.and_then(|c| text(c, "message"))
			.or_else(|| {
				commits
					.iter()
					.find(|c| after.is_some() && text(c, "id") == after)
					.or(commits.last())
					.and_then(|c| text(c, "message"))
			})
			.unwrap_or_default();
		let listed = ["added", "modified", "removed"];
		let has_lists = commits
			.iter()
			.any(|c| listed.iter().any(|k| c.get(*k).is_some()));
		let payload_files = has_lists.then(|| {
			let mut files: Vec<String> = Vec::new();
			for c in &commits {
				for k in listed {
					for f in c
						.get(k)
						.and_then(Value::as_array)
						.into_iter()
						.flatten()
						.filter_map(Value::as_str)
					{
						if !files.iter().any(|x| x == f) {
							files.push(f.to_owned());
						}
					}
				}
			}
			files
		});
		Self {
			git_ref: text(&v, "ref").unwrap_or_default(),
			before: text(&v, "before"),
			after,
			message,
			commits: commits.iter().filter_map(|c| text(c, "id")).collect(),
			payload_files,
		}
	}

	/// A push that removed its ref.
	fn is_deletion(&self) -> bool {
		self.after
			.as_deref()
			.is_some_and(git_providers::api::is_zero_sha)
	}
}

/// What a push came to for one application.
#[derive(Debug)]
pub enum Outcome {
	/// A deployment was queued.
	Deployed(Deployment),
	/// Waiting for the commit's checks.
	Waiting,
	/// Held by a freeze window.
	Frozen,
	/// The commit message asked for no deploy.
	Skipped,
	/// No changed file passed the path filters.
	Filtered,
	/// The checks failed, or did not finish in time.
	Failed,
	/// Not a push this application deploys.
	Ignored,
}

/// Whether the ref of a push is one this application deploys.
fn ref_wanted(app: &Application, cfg: &Settings, legacy: bool, git_ref: &str) -> bool {
	let branch = branch_of(git_ref);
	if legacy {
		// The plain behaviour: an application cloned through a provider follows its branch;
		// any other sends what it likes.
		return git_ref.is_empty()
			|| app.git_provider_id.is_none()
			|| branch == Some(app.git_ref.as_str());
	}
	let tag = git_ref
		.strip_prefix("refs/tags/")
		.filter(|t| !t.is_empty() && cfg.deploy_tags);
	let Some(name) = branch.or(tag) else {
		return false;
	};
	if cfg.branch_patterns.is_empty() {
		name == app.git_ref
	} else {
		rules::ref_matches(&cfg.branch_patterns, name)
	}
}

/// The changed files: from the provider when the application has one, else what the payload
/// listed. `None` when neither is known, which lets the push through.
async fn changed_files(s: &AppState, app: &Application, push: &Push) -> Option<Vec<String>> {
	if let (Some(provider), Some(repo), Some(after)) = (
		app.git_provider_id,
		app.git_repo.as_deref(),
		push.after.as_deref().filter(|a| valid_sha(a)),
	) {
		match git_providers::service::changed_files(
			s,
			provider,
			repo,
			push.before.as_deref(),
			after,
			&push.commits,
		)
		.await
		{
			Ok(files) => return Some(files),
			Err(e) => tracing::warn!(
				application = app.id.0,
				error = %e,
				"could not read the changed files; using the payload's"
			),
		}
	}
	push.payload_files.clone()
}

async fn team_of(s: &AppState, app: &Application) -> Option<TeamId> {
	sqlx::query_scalar::<_, TeamId>("SELECT team_id FROM projects WHERE id = $1")
		.bind(app.project_id)
		.fetch_optional(&s.db)
		.await
		.ok()
		.flatten()
}

async fn app_path(s: &AppState, app: &Application) -> Option<String> {
	Some(format!(
		"/teams/{}/projects/{}/apps/{}",
		team_of(s, app).await?,
		app.project_id,
		app.id
	))
}

/// Posts the deploy's state on a commit, when the application asks for it and has a provider
/// to post to. A failure is logged and goes no further: the deploy matters more.
async fn post(
	s: &AppState,
	app: &Application,
	cfg: &Settings,
	sha: &str,
	state: StatusState,
	description: &str,
) {
	if !cfg.post_status || !valid_sha(sha) {
		return;
	}
	let (Some(provider), Some(repo)) = (app.git_provider_id, app.git_repo.as_deref()) else {
		return;
	};
	let link = match (&s.config.public_url, app_path(s, app).await) {
		(Some(base), Some(path)) => Some(format!("{}{path}", base.trim_end_matches('/'))),
		_ => None,
	};
	let post = StatusPost {
		state,
		description,
		link: link.as_deref(),
	};
	if let Err(e) = git_providers::service::post_status(s, provider, repo, sha, &post).await {
		tracing::warn!(application = app.id.0, error = %e, "could not post the commit status");
	}
}

/// Tells the team that a push was not deployed because of its checks.
async fn notify_not_deployed(s: &AppState, app: &Application, row: &PushRow, why: &str) {
	let Some(team) = team_of(s, app).await else {
		return;
	};
	let short: String = row.commit_sha.chars().take(7).collect();
	let mut event = Event::new(
		team,
		EventKind::DeployFailed,
		format!("app:{}", app.id.0),
		format!("{} was not deployed", app.name),
	)
	.body(super::clip(
		&format!("Commit {short} on {}: {why}", row.git_ref),
		500,
	))
	.scope(app.project_id, Some(app.environment_id));
	if let Some(path) = app_path(s, app).await {
		event = event.link(path);
	}
	notifications::emit(s, event).await;
}

/// Handles a push for one git application.
///
/// # Errors
/// A database error, or the deployment that could not be queued.
#[allow(
	clippy::items_after_statements,
	reason = "the helper sits next to its only use"
)]
pub async fn handle(s: &AppState, app: &Application, push: &Push) -> AppResult<Outcome> {
	debug_assert_eq!(app.source_kind, SourceKind::Git);
	let stored = super::settings(&s.db, app.id).await?;
	let legacy = stored.is_none();
	let cfg = stored.unwrap_or_default();
	if push.is_deletion() || !cfg.enabled || !ref_wanted(app, &cfg, legacy, &push.git_ref) {
		return Ok(Outcome::Ignored);
	}
	let sha = push.after.as_deref().unwrap_or_default();
	fn verdict<'a>(push: &'a Push, sha: &'a str, state: State, detail: &'a str) -> NewPush<'a> {
		NewPush {
			git_ref: &push.git_ref,
			sha,
			message: &push.message,
			state,
			detail,
			ci_deadline: None,
		}
	}
	let record = |state: State, detail: &'static str| verdict(push, sha, state, detail);

	if let Some(marker) = rules::skip_marker(&push.message) {
		let detail = format!("The commit message has {marker}.");
		super::record_push(&s.db, app.id, &verdict(push, sha, State::Skipped, &detail)).await?;
		super::prune(&s.db, app.id, KEEP).await?;
		return Ok(Outcome::Skipped);
	}

	if !cfg.path_filters.is_empty() {
		let files = changed_files(s, app, push).await;
		if !rules::paths_pass(&cfg.path_filters, files.as_deref()) {
			super::record_push(
				&s.db,
				app.id,
				&record(State::Filtered, "No changed file matches the path filters."),
			)
			.await?;
			super::prune(&s.db, app.id, KEEP).await?;
			return Ok(Outcome::Filtered);
		}
	}

	// From here the push will deploy: whatever an older push left waiting is replaced.
	let now = OffsetDateTime::now_utc();
	for old in super::supersede_pending(&s.db, app.id).await? {
		post(
			s,
			app,
			&cfg,
			&old.commit_sha,
			StatusState::Cancelled,
			"Replaced by a newer push.",
		)
		.await;
	}
	let waits = cfg.wait_for_ci && app.git_provider_id.is_some() && valid_sha(sha);
	let row = super::record_push(
		&s.db,
		app.id,
		&NewPush {
			state: if waits {
				State::WaitingCi
			} else {
				State::Frozen
			},
			detail: if waits {
				"Waiting for the commit's checks."
			} else {
				""
			},
			ci_deadline: waits.then(|| now + Duration::seconds(i64::from(cfg.ci_timeout_secs))),
			..record(State::Frozen, "")
		},
	)
	.await?;
	super::prune(&s.db, app.id, KEEP).await?;
	if waits {
		post(
			s,
			app,
			&cfg,
			sha,
			StatusState::Pending,
			"Waiting for the commit's checks.",
		)
		.await;
	}
	let outcome = match advance(s, row, now, None).await? {
		Advance::Released(d) => Outcome::Deployed(d),
		Advance::Held(State::Frozen) => Outcome::Frozen,
		Advance::Held(_) => Outcome::Waiting,
		Advance::Stopped => Outcome::Failed,
		Advance::Gone => Outcome::Ignored,
	};
	if matches!(outcome, Outcome::Waiting | Outcome::Frozen) {
		wake(s);
	}
	Ok(outcome)
}

/// Where one step left a waiting push.
#[derive(Debug)]
pub enum Advance {
	Released(Deployment),
	Held(State),
	/// The checks failed or ran out of time, and nothing was deployed.
	Stopped,
	/// Someone else released it, or it was removed.
	Gone,
}

/// One step for a waiting push: read the checks, look at the freeze windows, and release it
/// when both allow. `force` is the maintainer's: it skips both. `by` is who forced it.
///
/// # Errors
/// A database error, or the deployment that could not be queued.
pub async fn advance(
	s: &AppState,
	row: PushRow,
	now: OffsetDateTime,
	force: Option<UserId>,
) -> AppResult<Advance> {
	let Some(app) =
		crate::applications::find_by_id(&s.db, ApplicationId(row.application_id)).await?
	else {
		return Ok(Advance::Gone);
	};
	let cfg = super::settings(&s.db, app.id).await?.unwrap_or_default();
	let state = row.state();
	let mut note = String::new();

	if force.is_none() && state == Some(State::WaitingCi) {
		match check_ci(s, &app, &row, &cfg, now).await {
			CiStep::Wait(detail) => {
				if detail != row.detail {
					super::set_detail(&s.db, row.id, &detail).await?;
				}
				return Ok(Advance::Held(State::WaitingCi));
			}
			CiStep::Go(n) => note = n,
			CiStep::Stop(stop, detail) => {
				super::set_state(&s.db, row.id, stop, &detail).await?;
				notify_not_deployed(s, &app, &row, &detail).await;
				post(
					s,
					&app,
					&cfg,
					&row.commit_sha,
					StatusState::Failure,
					&detail,
				)
				.await;
				return Ok(Advance::Stopped);
			}
		}
	}

	if force.is_none() {
		let windows = super::windows(&s.db, app.id).await?;
		if rules::frozen_at(&windows, now) {
			let detail = "Held by a freeze window. It deploys when the window ends.";
			if state != Some(State::Frozen) || row.detail != detail {
				super::set_state(&s.db, row.id, State::Frozen, detail).await?;
			}
			if state != Some(State::Frozen) || row.detail.is_empty() {
				post(
					s,
					&app,
					&cfg,
					&row.commit_sha,
					StatusState::Pending,
					"Held by a freeze window.",
				)
				.await;
			}
			return Ok(Advance::Held(State::Frozen));
		}
	}

	let Some(claimed) = super::claim(&s.db, row.id).await? else {
		return Ok(Advance::Gone);
	};
	let detail = match (force.is_some(), note.is_empty()) {
		(true, _) => "Deployed by a maintainer, without waiting.".to_owned(),
		(false, true) => "Deployed.".to_owned(),
		(false, false) => note,
	};
	match release(s, &app, &cfg, &claimed, force, &detail).await {
		Ok(d) => Ok(Advance::Released(d)),
		Err(e) => {
			// Put it back, so the next pass tries again.
			super::set_state(
				&s.db,
				claimed.id,
				row.state().unwrap_or(State::Frozen),
				&format!("Could not start the deploy: {e}"),
			)
			.await?;
			Err(e)
		}
	}
}

enum CiStep {
	Wait(String),
	Go(String),
	Stop(State, String),
}

async fn check_ci(
	s: &AppState,
	app: &Application,
	row: &PushRow,
	cfg: &Settings,
	now: OffsetDateTime,
) -> CiStep {
	let (Some(provider), Some(repo)) = (app.git_provider_id, app.git_repo.as_deref()) else {
		return CiStep::Go("No git provider, so the checks were not read.".to_owned());
	};
	let timed_out = row.ci_deadline.is_some_and(|d| now >= d);
	let timeout = || {
		if cfg.deploy_on_ci_timeout {
			CiStep::Go("The checks did not finish in time; deployed anyway.".to_owned())
		} else {
			CiStep::Stop(
				State::CiTimeout,
				"The checks did not finish in time. Nothing was deployed.".to_owned(),
			)
		}
	};
	match git_providers::service::ci(s, provider, repo, &row.commit_sha).await {
		Ok(Ci::Pass) => CiStep::Go("The checks passed.".to_owned()),
		Ok(Ci::Fail(name)) => CiStep::Stop(
			State::CiFailed,
			format!("{name} failed. Nothing was deployed."),
		),
		Ok(Ci::Pending | Ci::NoChecks) | Err(_) if timed_out => timeout(),
		Ok(Ci::Pending) => CiStep::Wait("Waiting for the commit's checks.".to_owned()),
		Ok(Ci::NoChecks) if now >= row.created_at + NO_CHECKS_GRACE => {
			CiStep::Go("The provider reports no checks for the commit.".to_owned())
		}
		Ok(Ci::NoChecks) => CiStep::Wait("Waiting for the first check to appear.".to_owned()),
		Err(e) => CiStep::Wait(format!("Could not read the checks yet: {e}")),
	}
}

/// Queues the deployment for a claimed push, first replacing the deployments an older push
/// left that have not started deploying.
async fn release(
	s: &AppState,
	app: &Application,
	cfg: &Settings,
	row: &PushRow,
	by: Option<UserId>,
	detail: &str,
) -> AppResult<Deployment> {
	for old in super::not_started(&s.db, app.id).await? {
		let Some(id) = old.deployment_id else {
			continue;
		};
		let cancelled = match jobs::latest_for(&s.db, JobKind::Deploy, id).await? {
			Some(job) => jobs::worker::cancel(s, job.id).await?,
			None => false,
		};
		if cancelled {
			super::set_state(
				&s.db,
				old.id,
				State::Superseded,
				"Replaced by a newer push before it finished.",
			)
			.await?;
			post(
				s,
				app,
				cfg,
				&old.commit_sha,
				StatusState::Cancelled,
				"Replaced by a newer push.",
			)
			.await;
		}
	}
	let deployment = deployments::queue(s, app, by, None).await?;
	super::link_deployment(&s.db, row.id, deployment.id.0, detail).await?;
	post(
		s,
		app,
		cfg,
		&row.commit_sha,
		StatusState::Pending,
		"Deploying with podkit.",
	)
	.await;
	Ok(deployment)
}

/// One pass over everything waiting, at `now`.
pub async fn process(s: &AppState, now: OffsetDateTime) {
	let rows = match super::pending(&s.db).await {
		Ok(rows) => rows,
		Err(e) => {
			tracing::warn!("cannot read the autodeploy queue: {e}");
			return;
		}
	};
	for row in rows {
		let id = row.id;
		if let Err(e) = advance(s, row, now, None).await {
			tracing::warn!(push = id, "autodeploy step failed: {e}");
		}
	}
}

/// Makes sure the pass runs while something waits. Cheap to call; one loop at a time, and it
/// ends by itself when the queue is empty. Does nothing under test, where the tests call
/// [`process`] with their own clock.
pub fn wake(s: &AppState) {
	#[cfg(not(test))]
	{
		if RUNNING.swap(true, Ordering::SeqCst) {
			return;
		}
		let s = s.clone();
		tokio::spawn(async move {
			loop {
				process(&s, OffsetDateTime::now_utc()).await;
				if super::pending(&s.db)
					.await
					.is_ok_and(|rows| rows.is_empty())
				{
					RUNNING.store(false, Ordering::SeqCst);
					// A push recorded between the look and the store would have found the
					// loop running and left it.
					let again = super::pending(&s.db)
						.await
						.is_ok_and(|rows| !rows.is_empty());
					if !again || RUNNING.swap(true, Ordering::SeqCst) {
						break;
					}
				}
				tokio::time::sleep(POLL).await;
			}
		});
	}
	#[cfg(test)]
	{
		let _ = s;
	}
}

/// Called when a deploy job ends: posts the result on the commit that caused it.
pub fn deploy_finished(s: &AppState, job: &Job, result: &Result<(), JobFailure>) {
	if job.kind != JobKind::Deploy {
		return;
	}
	let (state, text) = match result {
		Ok(()) => (StatusState::Success, "Deployed with podkit."),
		Err(JobFailure::Cancelled) => (StatusState::Cancelled, "The deploy was cancelled."),
		Err(JobFailure::Failed(_)) => (StatusState::Failure, "The deploy failed."),
	};
	let (s, deployment) = (s.clone(), job.target_id);
	tokio::spawn(async move {
		post_result(&s, deployment, state, text).await;
	});
}

/// Posts a deployment's result on the commit of the push that made it.
pub async fn post_result(s: &AppState, deployment: i64, state: StatusState, text: &str) {
	let Ok(Some(row)) = super::by_deployment(&s.db, deployment).await else {
		return;
	};
	// A deployment a newer push replaced already had its status posted.
	if row.state() != Some(State::Deployed) {
		return;
	}
	let Ok(Some(app)) =
		crate::applications::find_by_id(&s.db, ApplicationId(row.application_id)).await
	else {
		return;
	};
	let Ok(cfg) = super::settings(&s.db, app.id).await else {
		return;
	};
	post(
		s,
		&app,
		&cfg.unwrap_or_default(),
		&row.commit_sha,
		state,
		text,
	)
	.await;
}
