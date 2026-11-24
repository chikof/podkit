//! Previews' use cases: what a pull request event does, what the settings page reads and
//! writes, and the sweep that removes previews nobody uses.
//!
//! Events arrive from a webhook that is already authenticated for an application (its own
//! secret, or the GitHub App's signature) and act as podkit itself ([`audit::system`]).
//! The settings and the manual removal are for people: they authorize, then scope, then write.

use sqlx::PgPool;

use super::event::{self, Event, Pr};
use super::{Preview, PreviewId, Settings, State, build, comment};
use crate::applications::{self, Application, SourceKind};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::git_providers::Kind;
use crate::git_providers::service as providers;
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::notifications::{self, Event as Notice, EventKind};
use crate::scope;
use crate::state::AppState;

/// What an event came to, for the webhook's answer and for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
	/// Previews are off for the application, or the event is not about it.
	Ignored,
	/// A preview is being made or redeployed.
	Building(PreviewId),
	/// A fork's pull request waits for a maintainer's `/preview`.
	AwaitingApproval(PreviewId),
	/// The application's limit of previews is reached.
	Limited(PreviewId),
	/// A preview is being taken down, or there was none to take down.
	Removing,
}

/// The settings of an application whose previews are on, and the repository it follows.
async fn enabled(s: &AppState, app: &Application) -> AppResult<Option<Settings>> {
	if app.source_kind != SourceKind::Git || app.git_provider_id.is_none() || app.git_repo.is_none()
	{
		return Ok(None);
	}
	Ok(super::settings(&s.db, app.id)
		.await?
		.filter(|settings| settings.enabled))
}

fn same_repo(a: &str, b: Option<&str>) -> bool {
	b.is_some_and(|b| a.eq_ignore_ascii_case(b))
}

/// Queues the job that brings `preview` up (or redeploys it) or takes it down.
async fn queue(s: &AppState, app: &Application, preview: PreviewId) -> AppResult<()> {
	jobs::enqueue(
		s,
		JobKind::Preview,
		app.server_id,
		preview.0,
		format!("preview:{preview}"),
	)
	.await?;
	Ok(())
}

async fn record(s: &AppState, app: &Application, preview: &Preview, action: &str) {
	audit::record(
		&s.db,
		&audit::system(preview.team_id),
		action,
		Target::new("preview", preview.id.0)
			.named(format!("{}#{}", app.slug, preview.number))
			.in_project(app.project_id),
		Diff::new()
			.set("application", app.name.clone())
			.set("pull_request", i64::from(preview.number))
			.set("fork", preview.from_fork),
	)
	.await;
}

/// Tells the team a preview could not be made.
pub async fn notify_failed(s: &AppState, app: &Application, preview: &Preview, why: &str) {
	let event = Notice::new(
		preview.team_id,
		EventKind::PreviewFailed,
		format!("preview:{}", preview.id),
		format!("The preview of {} #{} failed", app.name, preview.number),
	)
	.body(why.chars().take(500).collect::<String>())
	.scope(app.project_id, Some(app.environment_id))
	.link(format!(
		"/teams/{}/projects/{}/apps/{}/git",
		preview.team_id, app.project_id, app.id
	));
	notifications::emit(s, event).await;
}

/// Makes the environment and queues the job, or says why not: a preview that does not fit
/// under the limit waits as `limited`.
async fn admit(
	s: &AppState,
	app: &Application,
	settings: &Settings,
	preview: &Preview,
) -> AppResult<Outcome> {
	if preview.environment_id.is_some() {
		super::set_state(&s.db, preview.id, State::Queued, "").await?;
		queue(s, app, preview.id).await?;
		return Ok(Outcome::Building(preview.id));
	}
	if super::live_count(&s.db, app.id).await? >= i64::from(settings.max_concurrent) {
		super::set_state(&s.db, preview.id, State::Limited, "").await?;
		return Ok(Outcome::Limited(preview.id));
	}
	match build::make(s, preview, app, settings).await {
		Ok(()) => {
			queue(s, app, preview.id).await?;
			record(s, app, preview, "preview.create").await;
			Ok(Outcome::Building(preview.id))
		}
		Err(e) => {
			let why = e.to_string();
			super::set_state(&s.db, preview.id, State::Failed, &why).await?;
			notify_failed(s, app, preview, &why).await;
			Ok(Outcome::Building(preview.id))
		}
	}
}

async fn insert(s: &AppState, app: &Application, pr: &Pr) -> AppResult<Preview> {
	let id = PreviewId::generate();
	let number = i32::try_from(pr.number).map_err(|_| {
		ServerError::Validation("That pull request number is too large.".to_owned())
	})?;
	let team: TeamId = sqlx::query_scalar("SELECT team_id FROM projects WHERE id = $1")
		.bind(app.project_id)
		.fetch_one(&s.db)
		.await?;
	sqlx::query(
		"INSERT INTO previews
			(id, team_id, project_id, application_id, number, title, head_sha, head_ref,
			 head_repo, from_fork, state)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'queued')
		ON CONFLICT (application_id, number) DO NOTHING",
	)
	.bind(id)
	.bind(team)
	.bind(app.project_id)
	.bind(app.id)
	.bind(number)
	.bind(pr.title.chars().take(200).collect::<String>())
	.bind(&pr.head_sha)
	.bind(&pr.head_ref)
	.bind(&pr.head_repo)
	.bind(pr.is_fork())
	.execute(&s.db)
	.await?;
	super::find_for_pr(&s.db, app.id, number)
		.await?
		.ok_or(ServerError::Internal)
}

/// A pull request that was opened, reopened or pushed to.
async fn changed(
	s: &AppState,
	app: &Application,
	settings: &Settings,
	pr: &Pr,
	opened: bool,
) -> AppResult<Outcome> {
	// Pull requests against another branch than the one the application follows are not its.
	if opened && pr.base_ref != app.git_ref {
		return Ok(Outcome::Ignored);
	}
	let number = i32::try_from(pr.number).map_err(|_| ServerError::Internal)?;
	let existing = super::find_for_pr(&s.db, app.id, number).await?;
	let fresh = existing.is_none();
	let preview = match existing {
		Some(p) if p.state() == State::Removing => return Ok(Outcome::Removing),
		Some(p) => p,
		None => insert(s, app, pr).await?,
	};
	// New commits: the code is not the one a maintainer approved.
	let moved = preview.head_sha != pr.head_sha;
	let fork = preview.from_fork || pr.is_fork();
	let revoked = preview.approved && fork && moved;
	let approved = preview.approved && !revoked;
	sqlx::query(
		"UPDATE previews SET title = $2, head_sha = $3, head_ref = $4, head_repo = $5,
			from_fork = $6, approved = $7, last_activity_at = NOW()
		WHERE id = $1",
	)
	.bind(preview.id)
	.bind(pr.title.chars().take(200).collect::<String>())
	.bind(&pr.head_sha)
	.bind(&pr.head_ref)
	.bind(&pr.head_repo)
	.bind(pr.is_fork())
	.bind(approved)
	.execute(&s.db)
	.await?;
	let preview = super::find_by_id(&s.db, preview.id)
		.await?
		.ok_or(ServerError::Internal)?;
	if revoked {
		// `approved_sha` stays: it is the commit that may still be running, and what the
		// comment says it serves. Nothing is built from the new head until `/preview`.
		record(s, app, &preview, "preview.revoke").await;
	}
	let outcome = if preview.from_fork && !preview.approved {
		super::set_state(&s.db, preview.id, State::AwaitingApproval, "").await?;
		Outcome::AwaitingApproval(preview.id)
	} else if !fresh && !moved && preview.environment_id.is_some() && !opened {
		// A delivery that brought nothing new.
		Outcome::Building(preview.id)
	} else {
		admit(s, app, settings, &preview).await?
	};
	if let Some(now) = super::find_by_id(&s.db, preview.id).await? {
		comment::publish(s, &now, app, settings.max_concurrent, settings.ttl_days).await;
	}
	Ok(outcome)
}

/// A pull request closed or merged: its preview goes.
async fn closed(s: &AppState, app: &Application, number: i64) -> AppResult<Outcome> {
	let Ok(number) = i32::try_from(number) else {
		return Ok(Outcome::Ignored);
	};
	let Some(preview) = super::find_for_pr(&s.db, app.id, number).await? else {
		return Ok(Outcome::Ignored);
	};
	begin_removal(s, app, &preview).await?;
	Ok(Outcome::Removing)
}

/// Starts taking `preview` down. A preview that never got an environment is simply forgotten
/// (after its comment says so).
async fn begin_removal(s: &AppState, app: &Application, preview: &Preview) -> AppResult<()> {
	let ttl = super::settings(&s.db, app.id)
		.await?
		.unwrap_or_default()
		.ttl_days;
	super::set_state(&s.db, preview.id, State::Removing, "").await?;
	let Some(current) = super::find_by_id(&s.db, preview.id).await? else {
		return Ok(());
	};
	if current.environment_id.is_none() {
		comment::publish(s, &current, app, 0, ttl).await;
		super::delete(&s.db, current.id).await?;
		return Ok(());
	}
	record(s, app, &current, "preview.remove").await;
	queue(s, app, current.id).await?;
	Ok(())
}

/// A comment on a pull request: `/preview` from someone who may write to the repository
/// approves a fork's preview and (re)builds any other.
async fn commented(
	s: &AppState,
	app: &Application,
	settings: &Settings,
	number: i64,
	body: &str,
	login: &str,
	user_id: Option<i64>,
) -> AppResult<Outcome> {
	if !event::asks_for_preview(body) {
		return Ok(Outcome::Ignored);
	}
	let (Some(provider), Some(repo)) = (app.git_provider_id, app.git_repo.as_deref()) else {
		return Ok(Outcome::Ignored);
	};
	// Asked of the provider, not read from the payload: anyone may comment on a public pull
	// request, and the payload is what the sender says.
	match providers::can_write(s, provider, repo, login, user_id).await {
		Ok(true) => {}
		Ok(false) => return Ok(Outcome::Ignored),
		Err(e) => {
			tracing::warn!(application = app.id.0, error = %e, "cannot tell who commented /preview");
			return Ok(Outcome::Ignored);
		}
	}
	let pull = providers::pull_request(s, provider, repo, number).await?;
	if !pull.open {
		return Ok(Outcome::Ignored);
	}
	let pr = Pr {
		repo: repo.to_owned(),
		number: pull.number,
		title: pull.title,
		head_sha: pull.head_sha,
		head_ref: pull.head_ref,
		head_repo: pull.head_repo,
		base_ref: pull.base_ref,
	};
	let n = i32::try_from(pr.number).map_err(|_| ServerError::Internal)?;
	let existing = super::find_for_pr(&s.db, app.id, n).await?;
	let preview = match existing {
		Some(p) if p.state() == State::Removing => return Ok(Outcome::Removing),
		Some(p) => p,
		None => insert(s, app, &pr).await?,
	};
	sqlx::query(
		"UPDATE previews SET title = $2, head_sha = $3, head_ref = $4, head_repo = $5,
			from_fork = $6, approved = TRUE, approved_sha = $3, last_activity_at = NOW()
		WHERE id = $1",
	)
	.bind(preview.id)
	.bind(pr.title.chars().take(200).collect::<String>())
	.bind(&pr.head_sha)
	.bind(&pr.head_ref)
	.bind(&pr.head_repo)
	.bind(pr.is_fork())
	.execute(&s.db)
	.await?;
	let preview = super::find_by_id(&s.db, preview.id)
		.await?
		.ok_or(ServerError::Internal)?;
	let outcome = admit(s, app, settings, &preview).await?;
	if let Some(now) = super::find_by_id(&s.db, preview.id).await? {
		comment::publish(s, &now, app, settings.max_concurrent, settings.ttl_days).await;
	}
	Ok(outcome)
}

/// One delivery for one application. `Ignored` for an application whose previews are off,
/// or a pull request that is not for it.
///
/// # Errors
/// The database fails, or the provider could not be asked.
pub async fn handle(s: &AppState, app: &Application, event: &Event) -> AppResult<Outcome> {
	let Some(settings) = enabled(s, app).await? else {
		return Ok(Outcome::Ignored);
	};
	match event {
		Event::Opened(pr) | Event::Synced(pr) => {
			if !same_repo(&pr.repo, app.git_repo.as_deref()) {
				return Ok(Outcome::Ignored);
			}
			changed(s, app, &settings, pr, matches!(event, Event::Opened(_))).await
		}
		Event::Closed { pr, .. } => {
			if !same_repo(&pr.repo, app.git_repo.as_deref()) {
				return Ok(Outcome::Ignored);
			}
			closed(s, app, pr.number).await
		}
		Event::Comment {
			repo,
			number,
			body,
			login,
			user_id,
		} => {
			if !same_repo(repo, app.git_repo.as_deref()) {
				return Ok(Outcome::Ignored);
			}
			commented(s, app, &settings, *number, body, login, *user_id).await
		}
	}
}

/// A delivery that came from the provider of kind `kind`, for every application of the team
/// that follows `repo` through `provider`. Used by the GitHub App's one webhook; a GitLab or
/// Gitea webhook belongs to one application and calls [`handle`] itself.
///
/// # Errors
/// As [`handle`].
pub async fn handle_for_repo(
	s: &AppState,
	provider: crate::ids::GitProviderId,
	kind: Kind,
	event_name: &str,
	body: &[u8],
) -> AppResult<usize> {
	let Some(event) = event::parse(kind, event_name, body) else {
		return Ok(0);
	};
	let repo = match &event {
		Event::Opened(pr) | Event::Synced(pr) | Event::Closed { pr, .. } => pr.repo.clone(),
		Event::Comment { repo, .. } => repo.clone(),
	};
	let mut handled = 0;
	for id in crate::git_providers::applications_for_push(&s.db, provider, &repo).await? {
		if let Some(app) = applications::find_by_id(&s.db, id).await?
			&& !matches!(handle(s, &app, &event).await?, Outcome::Ignored)
		{
			handled += 1;
		}
	}
	Ok(handled)
}

// ---- the page --------------------------------------------------------------------------

/// What the previews section of the Git tab shows.
#[derive(Debug, Clone)]
pub struct View {
	pub settings: Settings,
	pub previews: Vec<Preview>,
	/// Whether podkit can reach the provider to comment: the application is cloned through one.
	pub has_provider: bool,
	/// Variables of the application marked "available to previews".
	pub shared_variables: usize,
}

/// The settings and the previews of an application; needs `read application`.
///
/// # Errors
/// `Forbidden`, or `ApplicationNotFound` for an application of another team or project.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<View> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	Ok(View {
		settings: super::settings(&s.db, app.id).await?.unwrap_or_default(),
		previews: super::list_for_app(&s.db, app.id).await?,
		has_provider: app.git_provider_id.is_some() && app.git_repo.is_some(),
		shared_variables: super::preview_keys(&s.db, app.id).await?.len(),
	})
}

/// The keys of the application's variables that previews may read; needs `read env_var`.
///
/// # Errors
/// `Forbidden`, or `ApplicationNotFound` for an application of another team or project.
pub async fn available_keys(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<std::collections::HashSet<String>> {
	authorize(&s.db, user, team, Action::Read, Resource::EnvVar).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	Ok(super::preview_keys(&s.db, app.id).await?)
}

/// The environments of a project that are previews (for the "preview" badge). The caller
/// has already listed the project's environments, which is the check that it may.
///
/// # Errors
/// A database error.
pub async fn environment_ids(
	s: &AppState,
	team: TeamId,
	project: ProjectId,
) -> AppResult<std::collections::HashSet<crate::ids::EnvironmentId>> {
	Ok(super::environment_ids(&s.db, team, project).await?)
}

/// What the form sends.
#[derive(Debug, Clone, Default)]
pub struct SettingsInput {
	pub enabled: bool,
	pub command: String,
	pub max_concurrent: String,
	pub ttl_days: String,
	pub memory_limit_mb: String,
	pub cpu_limit: String,
	pub fork_variables: bool,
}

fn number<T: std::str::FromStr + PartialOrd + Copy>(
	text: &str,
	what: &str,
	range: std::ops::RangeInclusive<T>,
	fallback: Option<T>,
) -> Result<Option<T>, ServerError> {
	let text = text.trim();
	if text.is_empty() {
		return Ok(fallback);
	}
	match text.parse::<T>() {
		Ok(n) if range.contains(&n) => Ok(Some(n)),
		_ => Err(ServerError::Validation(format!(
			"{what} is not a valid number."
		))),
	}
}

impl SettingsInput {
	/// The settings the form asks for.
	///
	/// # Errors
	/// `Validation` naming the field.
	pub fn parse(&self) -> Result<Settings, ServerError> {
		let command = self.command.trim().to_owned();
		if command.chars().count() > 500 || command.chars().any(|c| c == '\0') {
			return Err(ServerError::Validation(
				"The command is too long (500 characters at most).".to_owned(),
			));
		}
		Ok(Settings {
			enabled: self.enabled,
			command,
			max_concurrent: number(
				&self.max_concurrent,
				"Open previews",
				1..=20,
				Some(super::DEFAULT_MAX),
			)?
			.unwrap_or(super::DEFAULT_MAX),
			ttl_days: number(
				&self.ttl_days,
				"Days",
				1..=90,
				Some(super::DEFAULT_TTL_DAYS),
			)?
			.unwrap_or(super::DEFAULT_TTL_DAYS),
			memory_limit_mb: number(&self.memory_limit_mb, "Memory", 64..=1_048_576, None)?,
			cpu_limit: number(&self.cpu_limit, "CPU", 0.1..=256.0, None)?,
			fork_variables: self.fork_variables,
		})
	}
}

/// Saves the settings; needs `update application`. Turning previews on makes sure the
/// application's webhook at the provider also sends pull request events.
///
/// # Errors
/// `Forbidden`, `ApplicationNotFound`, or `Validation` for a field or an application that is
/// not cloned through a provider.
pub async fn save(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	input: &SettingsInput,
) -> AppResult<Settings> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let settings = input.parse()?;
	if settings.enabled && (app.source_kind != SourceKind::Git || app.git_provider_id.is_none()) {
		return Err(ServerError::Validation(
			"Previews need an application cloned through a git provider, so podkit can comment on the pull request."
				.to_owned(),
		));
	}
	let before = super::settings(&s.db, app.id).await?.unwrap_or_default();
	super::save_settings(&s.db, app.id, &settings).await?;
	if settings.enabled && !before.enabled {
		providers::refresh_hook(s, &app).await;
	}
	audit::log(
		&s.db,
		user,
		team,
		"preview.settings",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.changed("enabled", before.enabled, settings.enabled)
			.changed(
				"max_concurrent",
				before.max_concurrent,
				settings.max_concurrent,
			)
			.changed("ttl_days", before.ttl_days, settings.ttl_days)
			.changed(
				"fork_variables",
				before.fork_variables,
				settings.fork_variables,
			)
			.changed("command", before.command.clone(), settings.command.clone()),
	)
	.await;
	Ok(settings)
}

/// Takes a preview down by hand; needs `delete application`. A preview of another team or
/// application is `ApplicationNotFound`.
///
/// # Errors
/// As above.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: PreviewId,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Delete, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let preview = super::find_scoped(&s.db, team, project, id)
		.await?
		.filter(|p| p.application_id == app.id)
		.ok_or(ServerError::ApplicationNotFound)?;
	begin_removal(s, &app, &preview).await?;
	audit::log(
		&s.db,
		user,
		team,
		"preview.remove",
		Target::new("preview", preview.id.0)
			.named(format!("{}#{}", app.slug, preview.number))
			.in_project(project),
		Diff::new().set("pull_request", i64::from(preview.number)),
	)
	.await;
	Ok(())
}

/// Marks an application's variable as available to previews, or not; needs `update
/// application`.
///
/// # Errors
/// `Forbidden`, `ApplicationNotFound`, `EnvVarNotFound`.
pub async fn set_variable(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	var: crate::ids::EnvVarId,
	available: bool,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	if !super::set_var_preview(&s.db, app.id, var, available).await? {
		return Err(ServerError::EnvVarNotFound);
	}
	audit::log(
		&s.db,
		user,
		team,
		"preview.variable",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("available", available),
	)
	.await;
	Ok(())
}

// ---- the sweep -------------------------------------------------------------------------

/// Removes previews with no activity for their application's days, and starts the removal of
/// one that was left half removed. Runs from the hourly housekeeping; with no previews it is
/// one query.
pub async fn sweep(s: &AppState) {
	if let Err(e) = sweep_inner(s).await {
		tracing::warn!("preview sweep failed: {e}");
	}
}

async fn sweep_inner(s: &AppState) -> AppResult<()> {
	let expired: Vec<(PreviewId, ApplicationId)> = sqlx::query_as(
		"SELECT p.id, p.application_id FROM previews p
		LEFT JOIN preview_settings s ON s.application_id = p.application_id
		WHERE p.state <> 'removing'
			AND p.last_activity_at < NOW() - make_interval(days => COALESCE(s.ttl_days, $1))",
	)
	.bind(super::DEFAULT_TTL_DAYS)
	.fetch_all(&s.db)
	.await?;
	for (id, app) in expired {
		let (Some(preview), Some(app)) = (
			super::find_by_id(&s.db, id).await?,
			applications::find_by_id(&s.db, app).await?,
		) else {
			continue;
		};
		tracing::info!(preview = id.0, "preview expired");
		begin_removal(s, &app, &preview).await?;
	}
	// A removal whose job is gone (the server restarted, the job failed): try again.
	let stuck: Vec<(PreviewId, ApplicationId)> = sqlx::query_as(
		"SELECT p.id, p.application_id FROM previews p
		WHERE p.state = 'removing' AND p.environment_id IS NOT NULL
			AND NOT EXISTS (
				SELECT 1 FROM jobs j WHERE j.kind = 'preview' AND j.target_id = p.id
					AND j.status IN ('queued', 'running'))",
	)
	.fetch_all(&s.db)
	.await?;
	for (id, app) in stuck {
		if let Some(app) = applications::find_by_id(&s.db, app).await? {
			queue(s, &app, id).await?;
		}
	}
	Ok(())
}

/// A pool is enough to look a preview up for the job.
#[allow(dead_code, reason = "kept next to the other lookups the job uses")]
pub(crate) async fn load(db: &PgPool, id: PreviewId) -> Result<Option<Preview>, sqlx::Error> {
	super::find_by_id(db, id).await
}
