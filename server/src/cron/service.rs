//! Scheduled-task use cases shared by the JSON API and the web UI: authorize, then scope,
//! then validate, then touch the database.
//!
//! Anything a person reads or changes goes through here, so another team's task answers
//! exactly like a missing one.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::limits::{MAX_BACKOFF_S, MAX_COMMAND, MAX_NAME, MAX_RETRIES, MAX_TIMEOUT_S};
use super::scheduler::MAX_QUEUED;
use super::store::{self, NewTask};
use super::{Concurrency, CronRun, CronTask, Kind, Trigger, schedule};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, CronRunId, CronTaskId, EnvironmentId, ProjectId, ServerId, TeamId, UserId,
};
use crate::state::AppState;
use crate::{jobs, scope, validate};

const NAME_CONSTRAINT: &str = "cron_tasks_environment_id_name_key";

/// A task with what its list row and page show next to it.
#[derive(Debug, Clone)]
pub struct TaskView {
	pub task: CronTask,
	/// The application's name, when the task has one.
	pub application: Option<String>,
	pub last_run: Option<CronRun>,
}

/// What a new task is made of.
#[derive(Debug, Clone)]
pub struct CreateInput {
	pub environment: EnvironmentId,
	pub name: String,
	pub kind: Kind,
	/// Required for `exec`. For `container` it lends its env, networks and image.
	pub application: Option<ApplicationId>,
	/// Where a `container` task without an application runs.
	pub server: Option<ServerId>,
	/// A `container` task runs this image instead of the application's.
	pub image: Option<String>,
	pub command: String,
	pub schedule: String,
	pub timezone: String,
	pub timeout_s: i32,
	pub concurrency: Concurrency,
	pub max_retries: i32,
	pub retry_backoff_s: i32,
	pub enabled: bool,
}

/// The fields of a task that can change; `None` leaves one as it is.
#[derive(Debug, Clone, Default)]
pub struct UpdateInput {
	pub name: Option<String>,
	/// An empty string removes the image, so a `container` task uses its application's.
	pub image: Option<String>,
	pub server: Option<ServerId>,
	pub command: Option<String>,
	pub schedule: Option<String>,
	pub timezone: Option<String>,
	pub timeout_s: Option<i32>,
	pub concurrency: Option<Concurrency>,
	pub max_retries: Option<i32>,
	pub retry_backoff_s: Option<i32>,
	pub enabled: Option<bool>,
}

/// `90 s`, `5 min`, `2 h`: the largest unit that divides the time evenly.
#[must_use]
pub fn duration(secs: u64) -> String {
	match secs {
		s if s >= 3600 && s % 3600 == 0 => format!("{} h", s / 3600),
		s if s >= 60 && s % 60 == 0 => format!("{} min", s / 60),
		s => format!("{s} s"),
	}
}

fn invalid(message: impl Into<String>) -> ServerError {
	ServerError::Validation(message.into())
}

fn clean_name(name: &str) -> AppResult<String> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
		return Err(invalid(format!(
			"the name must be 1 to {MAX_NAME} characters without control characters"
		)));
	}
	Ok(name.to_owned())
}

fn clean_command(command: &str) -> AppResult<String> {
	let command = command.trim();
	if command.is_empty() {
		return Err(invalid("the command cannot be empty"));
	}
	if command.len() > MAX_COMMAND || command.contains('\0') {
		return Err(invalid(format!(
			"the command must be at most {MAX_COMMAND} characters"
		)));
	}
	Ok(command.to_owned())
}

fn clean_image(image: Option<String>) -> AppResult<Option<String>> {
	let Some(image) = image.map(|i| i.trim().to_owned()).filter(|i| !i.is_empty()) else {
		return Ok(None);
	};
	if !validate::image_ref(&image) {
		return Err(invalid("that is not a valid image reference"));
	}
	Ok(Some(image))
}

fn check_numbers(timeout_s: i32, retries: i32, backoff_s: i32) -> AppResult<()> {
	if !(1..=MAX_TIMEOUT_S).contains(&timeout_s) {
		return Err(invalid(format!(
			"the timeout must be between 1 second and {} hours",
			MAX_TIMEOUT_S / 3600
		)));
	}
	if !(0..=MAX_RETRIES).contains(&retries) {
		return Err(invalid(format!(
			"retries must be between 0 and {MAX_RETRIES}"
		)));
	}
	if !(1..=MAX_BACKOFF_S).contains(&backoff_s) {
		return Err(invalid(format!(
			"the retry delay must be between 1 second and {} minutes",
			MAX_BACKOFF_S / 60
		)));
	}
	Ok(())
}

fn next_slot(task: &CronTask, now: OffsetDateTime) -> AppResult<Option<OffsetDateTime>> {
	if !task.enabled {
		return Ok(None);
	}
	schedule::next_after(&task.schedule, &task.timezone, now)
		.map(Some)
		.map_err(invalid)
}

fn name_taken(e: sqlx::Error) -> ServerError {
	if is_unique_violation(&e, NAME_CONSTRAINT) {
		ServerError::Conflict("a task with that name already exists in this environment".to_owned())
	} else {
		e.into()
	}
}

/// Adds a task; needs `create cron`.
///
/// # Errors
/// `Validation` for a field that is wrong, `Conflict` for a name already in the
/// environment, `NotFound` for an environment, application or server of another team.
pub async fn create(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	input: CreateInput,
) -> AppResult<CronTask> {
	authorize(db, user, team, Action::Create, Resource::Cron).await?;
	let env = scope::environment(db, team, project, input.environment).await?;
	let name = clean_name(&input.name)?;
	let command = clean_command(&input.command)?;
	let image = clean_image(input.image)?;
	check_numbers(input.timeout_s, input.max_retries, input.retry_backoff_s)?;
	schedule::validate(&input.schedule, &input.timezone).map_err(invalid)?;

	let application = match input.application {
		Some(id) => {
			let app = scope::application(db, team, project, id).await?;
			if app.environment_id != env.id {
				return Err(invalid("the application is in another environment"));
			}
			Some(app)
		}
		None => None,
	};
	let server = match (&application, input.kind) {
		(Some(app), _) => app.server_id,
		(None, Kind::Exec) => {
			return Err(invalid("a task that runs in an app needs an application"));
		}
		(None, Kind::Container) => {
			if image.is_none() {
				return Err(invalid(
					"a one-shot container needs an image or an application",
				));
			}
			let id = input
				.server
				.ok_or_else(|| invalid("pick the server the container runs on"))?;
			scope::server(db, team, id).await?.id
		}
	};
	if input.kind == Kind::Exec && image.is_some() {
		return Err(invalid(
			"a task that runs in an app has no image of its own",
		));
	}

	let now = OffsetDateTime::now_utc();
	let mut new = NewTask {
		project_id: project,
		environment_id: env.id,
		server_id: server,
		application_id: application.map(|a| a.id),
		name,
		kind: input.kind,
		image,
		command,
		schedule: input.schedule.trim().to_owned(),
		timezone: input.timezone.trim().to_owned(),
		timeout_s: input.timeout_s,
		concurrency: input.concurrency,
		max_retries: input.max_retries,
		retry_backoff_s: input.retry_backoff_s,
		enabled: input.enabled,
		next_run_at: None,
	};
	new.next_run_at = if new.enabled {
		Some(schedule::next_after(&new.schedule, &new.timezone, now).map_err(invalid)?)
	} else {
		None
	};
	store::insert_task(db, &new).await.map_err(name_taken)
}

/// The project's tasks, optionally narrowed to an environment or an application; needs
/// `read cron`.
///
/// # Errors
/// `NotFound` for a project of another team.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	environment: Option<EnvironmentId>,
	application: Option<ApplicationId>,
) -> AppResult<Vec<TaskView>> {
	authorize(db, user, team, Action::Read, Resource::Cron).await?;
	scope::project(db, team, project).await?;
	let tasks = store::list_tasks(db, project, environment, application).await?;
	let ids: Vec<CronTaskId> = tasks.iter().map(|t| t.id).collect();
	let mut last = store::latest_runs(db, &ids).await?;
	let names = application_names(db, &tasks).await?;
	Ok(tasks
		.into_iter()
		.map(|task| TaskView {
			application: task.application_id.and_then(|a| {
				names
					.iter()
					.find(|(id, _)| *id == a)
					.map(|(_, n)| n.clone())
			}),
			last_run: last.remove(&task.id),
			task,
		})
		.collect())
}

async fn application_names(
	db: &PgPool,
	tasks: &[CronTask],
) -> Result<Vec<(ApplicationId, String)>, sqlx::Error> {
	let ids: Vec<i64> = tasks
		.iter()
		.filter_map(|t| t.application_id.map(|a| a.0))
		.collect();
	if ids.is_empty() {
		return Ok(Vec::new());
	}
	sqlx::query_as("SELECT id, name FROM applications WHERE id = ANY($1)")
		.bind(ids)
		.fetch_all(db)
		.await
}

/// One task; needs `read cron`.
///
/// # Errors
/// `CronNotFound` when it is not in the team's project.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CronTaskId,
) -> AppResult<TaskView> {
	authorize(db, user, team, Action::Read, Resource::Cron).await?;
	let task = scope::cron_task(db, team, project, id).await?;
	let mut last = store::latest_runs(db, &[task.id]).await?;
	let application = application_names(db, std::slice::from_ref(&task))
		.await?
		.pop()
		.map(|(_, n)| n);
	Ok(TaskView {
		application,
		last_run: last.remove(&task.id),
		task,
	})
}

/// Changes a task; needs `update cron`. A schedule, time zone or pause change moves the
/// next run.
///
/// # Errors
/// As [`create`].
pub async fn update(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CronTaskId,
	input: UpdateInput,
) -> AppResult<CronTask> {
	authorize(db, user, team, Action::Update, Resource::Cron).await?;
	let mut task = scope::cron_task(db, team, project, id).await?;
	if let Some(name) = &input.name {
		task.name = clean_name(name)?;
	}
	if let Some(command) = &input.command {
		task.command = clean_command(command)?;
	}
	if let Some(image) = input.image {
		let image = clean_image(Some(image))?;
		if task.kind == Kind::Exec && image.is_some() {
			return Err(invalid(
				"a task that runs in an app has no image of its own",
			));
		}
		task.image = image;
		if task.kind == Kind::Container && task.image.is_none() && task.application_id.is_none() {
			return Err(invalid(
				"a one-shot container needs an image or an application",
			));
		}
	}
	if let Some(server) = input.server
		&& task.application_id.is_none()
	{
		task.server_id = scope::server(db, team, server).await?.id;
	}
	if let Some(schedule) = &input.schedule {
		task.schedule = schedule.trim().to_owned();
	}
	if let Some(timezone) = &input.timezone {
		task.timezone = timezone.trim().to_owned();
	}
	if let Some(v) = input.timeout_s {
		task.timeout_s = v;
	}
	if let Some(v) = input.concurrency {
		task.concurrency = v;
	}
	if let Some(v) = input.max_retries {
		task.max_retries = v;
	}
	if let Some(v) = input.retry_backoff_s {
		task.retry_backoff_s = v;
	}
	if let Some(v) = input.enabled {
		task.enabled = v;
	}
	check_numbers(task.timeout_s, task.max_retries, task.retry_backoff_s)?;
	schedule::validate(&task.schedule, &task.timezone).map_err(invalid)?;
	task.next_run_at = next_slot(&task, OffsetDateTime::now_utc())?;
	store::update_task(db, &task).await.map_err(name_taken)
}

/// Pauses or resumes a task; needs `update cron`.
///
/// # Errors
/// `CronNotFound`.
pub async fn set_enabled(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CronTaskId,
	enabled: bool,
) -> AppResult<CronTask> {
	update(
		db,
		user,
		team,
		project,
		id,
		UpdateInput {
			enabled: Some(enabled),
			..UpdateInput::default()
		},
	)
	.await
}

/// Deletes a task and its runs; a run still going is cancelled first. Needs `delete cron`.
///
/// # Errors
/// `CronNotFound`.
pub async fn remove(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CronTaskId,
) -> AppResult<CronTaskId> {
	authorize(&state.db, user, team, Action::Delete, Resource::Cron).await?;
	let task = scope::cron_task(&state.db, team, project, id).await?;
	let active: Vec<(i64,)> = sqlx::query_as(
		"SELECT job_id FROM cron_runs
		WHERE task_id = $1 AND status IN ('queued', 'running') AND job_id IS NOT NULL",
	)
	.bind(task.id)
	.fetch_all(&state.db)
	.await?;
	for (job,) in active {
		let _ = jobs::worker::cancel(state, crate::ids::JobId(job)).await?;
	}
	store::delete_task(&state.db, task.id).await?;
	Ok(task.id)
}

/// Starts a run now, whatever the schedule says; needs `update cron`. The concurrency
/// policy still applies: a `skip` task that is running refuses.
///
/// # Errors
/// `Conflict` when the policy turns it away.
pub async fn run_now(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CronTaskId,
) -> AppResult<CronRun> {
	authorize(&state.db, user, team, Action::Update, Resource::Cron).await?;
	let task = scope::cron_task(&state.db, team, project, id).await?;
	let (active, waiting) = store::active_counts(&state.db, task.id).await?;
	match task.concurrency {
		Concurrency::Skip if active > 0 => {
			return Err(ServerError::Conflict(
				"the task is already running".to_owned(),
			));
		}
		Concurrency::Queue if waiting >= MAX_QUEUED => {
			return Err(ServerError::Conflict(
				"too many runs are already waiting".to_owned(),
			));
		}
		_ => {}
	}
	let mut conn = state.db.acquire().await?;
	let now = OffsetDateTime::now_utc();
	let run = store::enqueue_run(&mut conn, &task, Trigger::Manual, 1, None, now).await?;
	state.jobs.notify.notify_one();
	Ok(run)
}

/// A task's runs, newest first; needs `read cron`.
///
/// # Errors
/// `CronNotFound`.
pub async fn runs(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CronTaskId,
	before: Option<CronRunId>,
	limit: i64,
) -> AppResult<Vec<CronRun>> {
	authorize(db, user, team, Action::Read, Resource::Cron).await?;
	let task = scope::cron_task(db, team, project, id).await?;
	Ok(store::list_runs(db, task.id, before, limit.clamp(1, 100)).await?)
}

/// One run of a task; needs `read cron`.
///
/// # Errors
/// `CronNotFound` when the task is not the team's or the run is not the task's.
pub async fn run(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	task: CronTaskId,
	run: CronRunId,
) -> AppResult<CronRun> {
	authorize(db, user, team, Action::Read, Resource::Cron).await?;
	let task = scope::cron_task(db, team, project, task).await?;
	store::run_by_id(db, run)
		.await?
		.filter(|r| r.task_id == task.id)
		.ok_or(ServerError::CronNotFound("run"))
}

/// Stops a queued or running run; needs `update cron`.
///
/// # Errors
/// `Conflict` when the run is already over.
pub async fn cancel(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	task: CronTaskId,
	run: CronRunId,
) -> AppResult<()> {
	authorize(&state.db, user, team, Action::Update, Resource::Cron).await?;
	let run = self::run(&state.db, user, team, project, task, run).await?;
	let job = run
		.job_id
		.filter(|_| run.status.is_active())
		.ok_or_else(|| ServerError::Conflict("the run is already over".to_owned()))?;
	if jobs::worker::cancel(state, job).await? {
		Ok(())
	} else {
		Err(ServerError::Conflict("the run is already over".to_owned()))
	}
}

/// The upcoming runs of a schedule for the forms; needs `read cron`.
///
/// # Errors
/// `Forbidden`. A schedule that does not parse is the inner `Err`, to show under the field.
pub async fn preview(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	schedule_text: &str,
	timezone: &str,
) -> AppResult<Result<Vec<OffsetDateTime>, String>> {
	authorize(db, user, team, Action::Read, Resource::Cron).await?;
	Ok(schedule::upcoming(
		schedule_text,
		timezone,
		OffsetDateTime::now_utc(),
		schedule::PREVIEW_RUNS,
	))
}
