//! Scheduled tasks under `/api/v1/teams/{team}/projects/{project}/cron`. The use cases are
//! in [`super::service`].

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::limits::{DEFAULT_BACKOFF_S, DEFAULT_TIMEOUT_S};
use super::service::{self, CreateInput, TaskView, UpdateInput};
use super::{Concurrency, CronRun, CronTask, Kind, RunStatus, Trigger};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{
	ApplicationId, CronRunId, CronTaskId, EnvironmentId, ProjectId, ServerId, TeamId,
};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/preview", get(preview))
		.route("/{task_id}", get(show).patch(update).delete(remove))
		.route("/{task_id}/run", post(run_now))
		.route("/{task_id}/pause", post(pause))
		.route("/{task_id}/resume", post(resume))
		.route("/{task_id}/runs", get(list_runs))
		.route("/{task_id}/runs/{run_id}", get(show_run))
		.route("/{task_id}/runs/{run_id}/cancel", post(cancel))
}

fn time_text(t: OffsetDateTime) -> String {
	t.format(&Rfc3339).unwrap_or_default()
}

#[derive(Serialize)]
pub struct TaskResponse {
	id: CronTaskId,
	environment_id: EnvironmentId,
	application_id: Option<ApplicationId>,
	application: Option<String>,
	server_id: ServerId,
	name: String,
	kind: Kind,
	image: Option<String>,
	command: String,
	schedule: String,
	timezone: String,
	timeout_s: i32,
	concurrency: Concurrency,
	max_retries: i32,
	retry_backoff_s: i32,
	enabled: bool,
	next_run_at: Option<String>,
	last_run: Option<RunResponse>,
}

impl From<&CronTask> for TaskResponse {
	fn from(t: &CronTask) -> Self {
		Self {
			id: t.id,
			environment_id: t.environment_id,
			application_id: t.application_id,
			application: None,
			server_id: t.server_id,
			name: t.name.clone(),
			kind: t.kind,
			image: t.image.clone(),
			command: t.command.clone(),
			schedule: t.schedule.clone(),
			timezone: t.timezone.clone(),
			timeout_s: t.timeout_s,
			concurrency: t.concurrency,
			max_retries: t.max_retries,
			retry_backoff_s: t.retry_backoff_s,
			enabled: t.enabled,
			next_run_at: t.next_run_at.map(time_text),
			last_run: None,
		}
	}
}

impl From<&TaskView> for TaskResponse {
	fn from(v: &TaskView) -> Self {
		Self {
			application: v.application.clone(),
			last_run: v.last_run.as_ref().map(RunResponse::from),
			..Self::from(&v.task)
		}
	}
}

#[derive(Serialize)]
pub struct RunResponse {
	id: CronRunId,
	task_id: CronTaskId,
	status: RunStatus,
	trigger: Trigger,
	attempt: i32,
	scheduled_for: Option<String>,
	created_at: String,
	started_at: Option<String>,
	finished_at: Option<String>,
	exit_code: Option<i32>,
	error: Option<String>,
}

impl From<&CronRun> for RunResponse {
	fn from(r: &CronRun) -> Self {
		Self {
			id: r.id,
			task_id: r.task_id,
			status: r.status,
			trigger: r.trigger,
			attempt: r.attempt,
			scheduled_for: r.scheduled_for.map(time_text),
			created_at: time_text(r.created_at),
			started_at: r.started_at.map(time_text),
			finished_at: r.finished_at.map(time_text),
			exit_code: r.exit_code,
			error: r.error.clone(),
		}
	}
}

#[derive(Deserialize)]
pub struct CreateRequest {
	environment_id: EnvironmentId,
	name: String,
	kind: Kind,
	#[serde(default)]
	application_id: Option<ApplicationId>,
	#[serde(default)]
	server_id: Option<ServerId>,
	#[serde(default)]
	image: Option<String>,
	command: String,
	schedule: String,
	#[serde(default = "utc")]
	timezone: String,
	#[serde(default = "default_timeout")]
	timeout_s: i32,
	#[serde(default = "default_concurrency")]
	concurrency: Concurrency,
	#[serde(default)]
	max_retries: i32,
	#[serde(default = "default_backoff")]
	retry_backoff_s: i32,
	#[serde(default = "yes")]
	enabled: bool,
}

fn utc() -> String {
	"UTC".to_owned()
}
fn default_timeout() -> i32 {
	DEFAULT_TIMEOUT_S
}
fn default_backoff() -> i32 {
	DEFAULT_BACKOFF_S
}
fn default_concurrency() -> Concurrency {
	Concurrency::Skip
}
fn yes() -> bool {
	true
}

/// Adds a task. Caller must hold `create cron` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<CreateRequest>,
) -> AppResult<(StatusCode, Json<TaskResponse>)> {
	let task = service::create(
		&s.db,
		user.id,
		team,
		project,
		CreateInput {
			environment: body.environment_id,
			name: body.name,
			kind: body.kind,
			application: body.application_id,
			server: body.server_id,
			image: body.image,
			command: body.command,
			schedule: body.schedule,
			timezone: body.timezone,
			timeout_s: body.timeout_s,
			concurrency: body.concurrency,
			max_retries: body.max_retries,
			retry_backoff_s: body.retry_backoff_s,
			enabled: body.enabled,
		},
	)
	.await?;
	Ok((StatusCode::CREATED, Json(TaskResponse::from(&task))))
}

#[derive(Deserialize)]
struct ListParams {
	environment_id: Option<EnvironmentId>,
	application_id: Option<ApplicationId>,
}

/// The project's tasks. Caller must hold `read cron`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<TaskResponse>>> {
	let tasks = service::list(
		&s.db,
		user.id,
		team,
		project,
		params.environment_id,
		params.application_id,
	)
	.await?;
	Ok(Json(tasks.iter().map(TaskResponse::from).collect()))
}

#[derive(Deserialize)]
struct PreviewParams {
	schedule: String,
	#[serde(default = "utc")]
	timezone: String,
}

#[derive(Serialize)]
struct PreviewResponse {
	runs: Vec<String>,
	error: Option<String>,
}

/// The next five runs of a schedule in a time zone, or why it is not valid. Caller must
/// hold `read cron`.
async fn preview(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, _project)): Path<(TeamId, ProjectId)>,
	Query(params): Query<PreviewParams>,
) -> AppResult<Json<PreviewResponse>> {
	let result = service::preview(&s.db, user.id, team, &params.schedule, &params.timezone).await?;
	Ok(Json(match result {
		Ok(runs) => PreviewResponse {
			runs: runs.into_iter().map(time_text).collect(),
			error: None,
		},
		Err(e) => PreviewResponse {
			runs: Vec::new(),
			error: Some(e),
		},
	}))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> AppResult<Json<TaskResponse>> {
	let view = service::get(&s.db, user.id, team, project, task).await?;
	Ok(Json(TaskResponse::from(&view)))
}

#[derive(Deserialize)]
struct UpdateRequest {
	name: Option<String>,
	/// Empty removes it.
	image: Option<String>,
	server_id: Option<ServerId>,
	command: Option<String>,
	schedule: Option<String>,
	timezone: Option<String>,
	timeout_s: Option<i32>,
	concurrency: Option<Concurrency>,
	max_retries: Option<i32>,
	retry_backoff_s: Option<i32>,
	enabled: Option<bool>,
}

/// Changes a task. Caller must hold `update cron`.
async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
	Json(body): Json<UpdateRequest>,
) -> AppResult<Json<TaskResponse>> {
	let task = service::update(
		&s.db,
		user.id,
		team,
		project,
		task,
		UpdateInput {
			name: body.name,
			image: body.image,
			server: body.server_id,
			command: body.command,
			schedule: body.schedule,
			timezone: body.timezone,
			timeout_s: body.timeout_s,
			concurrency: body.concurrency,
			max_retries: body.max_retries,
			retry_backoff_s: body.retry_backoff_s,
			enabled: body.enabled,
		},
	)
	.await?;
	Ok(Json(TaskResponse::from(&task)))
}

#[derive(Serialize)]
struct Deleted {
	id: CronTaskId,
}

/// Deletes a task and its runs. Caller must hold `delete cron`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> AppResult<Json<Deleted>> {
	let id = service::remove(&s, user.id, team, project, task).await?;
	Ok(Json(Deleted { id }))
}

/// Queues a run now and returns at once. Caller must hold `update cron`.
async fn run_now(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> AppResult<(StatusCode, Json<RunResponse>)> {
	let run = service::run_now(&s, user.id, team, project, task).await?;
	Ok((StatusCode::ACCEPTED, Json(RunResponse::from(&run))))
}

async fn pause(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> AppResult<Json<TaskResponse>> {
	let task = service::set_enabled(&s.db, user.id, team, project, task, false).await?;
	Ok(Json(TaskResponse::from(&task)))
}

async fn resume(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> AppResult<Json<TaskResponse>> {
	let task = service::set_enabled(&s.db, user.id, team, project, task, true).await?;
	Ok(Json(TaskResponse::from(&task)))
}

#[derive(Deserialize)]
struct RunsParams {
	limit: Option<i64>,
	before: Option<CronRunId>,
}

async fn list_runs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
	Query(params): Query<RunsParams>,
) -> AppResult<Json<Vec<RunResponse>>> {
	let runs = service::runs(
		&s.db,
		user.id,
		team,
		project,
		task,
		params.before,
		params.limit.unwrap_or(50),
	)
	.await?;
	Ok(Json(runs.iter().map(RunResponse::from).collect()))
}

async fn show_run(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task, run)): Path<(TeamId, ProjectId, CronTaskId, CronRunId)>,
) -> AppResult<Json<RunResponse>> {
	let run = service::run(&s.db, user.id, team, project, task, run).await?;
	Ok(Json(RunResponse::from(&run)))
}

/// Stops a queued or running run. Caller must hold `update cron`.
async fn cancel(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, task, run)): Path<(TeamId, ProjectId, CronTaskId, CronRunId)>,
) -> AppResult<StatusCode> {
	service::cancel(&s, user.id, team, project, task, run).await?;
	Ok(StatusCode::ACCEPTED)
}
