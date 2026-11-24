//! One check of an application's tag, and the loop that runs the due ones.
//!
//! A check asks the application's own server agent which digest the tag names now (the agent
//! speaks to the registry, the way its pulls do, with the saved registry login), compares it
//! with the digests of the image that runs, and when [`rules::decide`] says so queues a
//! deployment through [`deployments::queue_as`], the path every deploy takes. What became of
//! the previous auto-update is read first, so a digest that failed or was rolled back is held
//! back.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration as StdDuration;

use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::rules::{self, Checked, Decision, Facts, Outcome};
use super::{Record, Row};
use crate::applications::{self, Application, SourceKind};
use crate::deployments;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, TeamId, UserId};
use crate::notifications::{self, Event, EventKind};
use crate::state::AppState;
use crate::{autodeploy, registries, scope};

/// How often the loop looks for due checks.
const TICK: StdDuration = StdDuration::from_mins(1);
/// What a deploy started by a check is called in the audit log.
pub const TRIGGER: &str = "auto-update";

/// Applications being checked right now, so a scheduled check and a Check now never overlap.
static CHECKING: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

struct Busy(i64);

impl Busy {
	fn take(app: i64) -> Option<Self> {
		let mut set = CHECKING
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		set.insert(app).then_some(Self(app))
	}
}

impl Drop for Busy {
	fn drop(&mut self) {
		CHECKING
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.remove(&self.0);
	}
}

/// Who asked for a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
	Schedule,
	/// Check now: the maintenance window does not apply, freezes still do.
	Manual(UserId),
}

/// Runs the due checks once a minute until `shutdown`.
pub async fn run(state: AppState, shutdown: CancellationToken) {
	loop {
		sweep(&state, OffsetDateTime::now_utc()).await;
		tokio::select! {
			() = shutdown.cancelled() => return,
			() = tokio::time::sleep(TICK) => {}
		}
	}
}

/// One pass over the due checks, at `now`.
pub async fn sweep(s: &AppState, now: OffsetDateTime) {
	let due = match super::due(&s.db, now).await {
		Ok(due) => due,
		Err(e) => {
			tracing::warn!("auto-update cannot list due checks: {e}");
			return;
		}
	};
	for app in due {
		match super::claim(&s.db, app, now, now + rules::RETRY).await {
			Ok(true) => {}
			Ok(false) => continue,
			Err(e) => {
				tracing::warn!(application = app, "auto-update cannot claim a check: {e}");
				continue;
			}
		}
		if let Err(e) = check(s, ApplicationId(app), Trigger::Schedule).await {
			tracing::warn!(application = app, "auto-update check failed: {e}");
		}
	}
}

async fn team_of(s: &AppState, app: &Application) -> Result<Option<TeamId>, sqlx::Error> {
	sqlx::query_scalar::<_, TeamId>("SELECT team_id FROM projects WHERE id = $1")
		.bind(app.project_id)
		.fetch_optional(&s.db)
		.await
}

/// The registry login a pull of the application uses: its saved registry, or the login it
/// carries itself.
async fn auth_of(s: &AppState, app: &Application) -> AppResult<Option<proto::RegistryAuth>> {
	if let Some(id) = app.registry_id {
		return registries::service::auth_for(s, id).await.map(Some);
	}
	deployments::run::registry_auth(s, app).map_err(|_| {
		ServerError::Validation("The application's registry login cannot be read.".to_owned())
	})
}

/// Asks the application's server which digest its tag names, and what the running image is
/// known by.
async fn ask(
	s: &AppState,
	app: &Application,
	image: &str,
	running: Option<&str>,
) -> AppResult<proto::ImageDigestResult> {
	let team = team_of(s, app)
		.await?
		.ok_or(ServerError::ApplicationNotFound)?;
	let server = scope::server(&s.db, team, app.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let req = proto::ImageDigestRequest {
		reference: image.to_owned(),
		auth: auth_of(s, app).await?,
		local: running.map(str::to_owned),
	};
	let answer = client.image_digest(&req).await?;
	if !rules::valid_digest(&answer.digest) {
		return Err(ServerError::Validation(
			"The registry sent a digest podkit cannot read.".to_owned(),
		));
	}
	Ok(answer)
}

/// What became of a deployment, from its status and health gate state.
async fn outcome_of(s: &AppState, deployment: i64) -> Result<Option<Outcome>, sqlx::Error> {
	let row = sqlx::query_as::<_, (String, Option<String>)>(
		"SELECT d.status, h.state FROM deployments d \
		 LEFT JOIN deployment_health h ON h.deployment_id = d.id WHERE d.id = $1",
	)
	.bind(deployment)
	.fetch_optional(&s.db)
	.await?;
	Ok(row.map(|(status, health)| rules::outcome(&status, health.as_deref())))
}

/// Whether a deployment of the application is queued or under way.
async fn deploying(s: &AppState, app: ApplicationId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar::<_, bool>(
		"SELECT EXISTS (SELECT 1 FROM deployments WHERE application_id = $1 \
		 AND status IN ('queued', 'building', 'deploying'))",
	)
	.bind(app.0)
	.fetch_one(&s.db)
	.await
}

fn app_path(team: TeamId, app: &Application) -> String {
	format!(
		"/teams/{team}/projects/{}/apps/{}/webhooks",
		app.project_id, app.id
	)
}

/// Tells the team that a digest is held back after its auto-update went bad.
async fn notify_held_back(s: &AppState, app: &Application, digest: &str) {
	let Ok(Some(team)) = team_of(s, app).await else {
		return;
	};
	let event = Event::new(
		team,
		EventKind::DeployFailed,
		format!("app:{}", app.id.0),
		format!("{} auto-update held back", app.name),
	)
	.body(format!(
		"The image {} failed to deploy or was rolled back. Auto-update skips it until the tag names another image.",
		rules::short_digest(digest)
	))
	.scope(app.project_id, Some(app.environment_id))
	.link(app_path(team, app));
	notifications::emit(s, event).await;
}

fn detail(result: Checked, remote: &str) -> String {
	let remote = rules::short_digest(remote);
	match result {
		Checked::UpToDate => format!("The tag names {remote}, which is what runs."),
		Checked::Deployed => format!("The tag moved to {remote}. A deployment was started."),
		Checked::SkippedBad => format!(
			"The tag names {remote}, whose auto-update failed or was rolled back. It is skipped until the tag moves."
		),
		Checked::Busy => format!("The tag moved to {remote}. Waiting for the deploy in progress."),
		Checked::Frozen => format!("The tag moved to {remote}. Held by a freeze window."),
		Checked::OutsideWindow => {
			format!("The tag moved to {remote}. It deploys in the maintenance window.")
		}
		Checked::NotRunning => format!(
			"The tag names {remote}. The application is not running, so there is nothing to update."
		),
		Checked::Error => String::new(),
	}
}

/// The image reference an application follows, or why it has none.
fn followed_image(app: &Application) -> Result<String, &'static str> {
	let (SourceKind::Image, Some(image)) = (&app.source_kind, app.image.as_deref()) else {
		return Err("Only an application deployed from a registry image can be auto-updated.");
	};
	rules::trackable(image)?;
	Ok(image.to_owned())
}

/// Checks the application's tag now and acts on it. Returns the row as the check left it, or
/// `None` when the application has no auto-update row (or, for a scheduled check, it is off).
///
/// # Errors
/// [`ServerError::Conflict`] when a check of the application is already running, or a
/// database error. A registry or agent that cannot be asked is a result, not an error.
pub async fn check(
	s: &AppState,
	app_id: ApplicationId,
	trigger: Trigger,
) -> AppResult<Option<Row>> {
	let Some(_guard) = Busy::take(app_id.0) else {
		return Err(ServerError::Conflict(
			"A check of this application is already running.".to_owned(),
		));
	};
	let Some(row) = super::get(&s.db, app_id).await? else {
		return Ok(None);
	};
	if trigger == Trigger::Schedule && !row.enabled {
		return Ok(None);
	}
	let Some(app) = applications::find_by_id(&s.db, app_id).await? else {
		return Ok(None);
	};
	let now = OffsetDateTime::now_utc();
	let interval = row.interval();
	let window = row.window();
	let mut out = Record {
		at: now,
		result: Checked::Error,
		detail: String::new(),
		next: None,
		remote_digest: row.remote_digest.clone(),
		deployment_id: row.deployment_id,
		deployed_digest: row.deployed_digest.clone(),
		bad_digest: row.bad_digest.clone(),
	};
	let finish = |mut out: Record| {
		out.next = Some(rules::next_check(now, out.result.delay(interval), window));
		out
	};

	let image = match followed_image(&app) {
		Ok(image) => image,
		Err(why) => {
			why.clone_into(&mut out.detail);
			return Ok(super::record(&s.db, app.id, &finish(out)).await?);
		}
	};

	let running = deployments::latest_running(&s.db, app.id).await?;
	let recorded = running.as_ref().and_then(|d| d.image_tag.as_deref());
	let answer = match ask(s, &app, &image, recorded).await {
		Ok(answer) => answer,
		Err(e @ ServerError::Sqlx(_)) => return Err(e),
		Err(e) => {
			out.detail = format!("Could not check {image}: {e}");
			return Ok(super::record(&s.db, app.id, &finish(out)).await?);
		}
	};
	let remote = answer.digest;
	let running_digests = rules::running_digests(recorded, &answer.local_digests);

	let previous = match row.deployment_id {
		Some(id) => outcome_of(s, id).await?,
		None => None,
	};
	let deployed = row.deployed_digest.as_deref().zip(previous);
	let bad = rules::bad_digest(row.bad_digest.as_deref(), deployed, &remote);
	if bad.is_some()
		&& bad != row.bad_digest
		&& let Some(digest) = bad.as_deref()
	{
		notify_held_back(s, &app, digest).await;
	}

	let windows = autodeploy::windows(&s.db, app.id).await?;
	let facts = Facts {
		remote: &remote,
		running: &running_digests,
		app_running: running.is_some(),
		deployed,
		bad: bad.as_deref(),
		busy: deploying(s, app.id).await?,
		frozen: autodeploy::rules::frozen_at(&windows, now),
		in_window: matches!(trigger, Trigger::Manual(_)) || window.is_none_or(|w| w.open_at(now)),
	};
	let decision = rules::decide(&facts);
	out.result = decision.into();
	out.detail = detail(out.result, &remote);
	out.remote_digest = Some(remote.clone());
	out.bad_digest = bad;

	if decision == Decision::Deploy {
		let by = match trigger {
			Trigger::Manual(user) => Some(user),
			Trigger::Schedule => None,
		};
		match deployments::queue_as(s, &app, by, None, TRIGGER).await {
			Ok(deployment) => {
				out.deployment_id = Some(deployment.id.0);
				out.deployed_digest = Some(remote);
			}
			Err(e @ ServerError::Sqlx(_)) => return Err(e),
			Err(e) => {
				out.result = Checked::Error;
				out.detail = format!("The tag moved, but the deploy could not start: {e}");
			}
		}
	}
	Ok(super::record(&s.db, app.id, &finish(out)).await?)
}
