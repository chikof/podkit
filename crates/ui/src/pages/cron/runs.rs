//! A task's run history: the table (polled while a run is going), Run now, Cancel and the
//! log of one run.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};

use super::{run_dot, took};
use crate::components::{
	Size, Variant, button_class, card, card_content, card_section, empty_state, icon,
	log_viewer_titled, table,
};
use crate::extract::TeamCtx;
use crate::live::job_log;
use crate::pages::databases::when;
use crate::sse::js_string;
use crate::status::poll;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::cron::service;
use podkit_core::cron::{CronRun, CronTask, RunStatus};
use podkit_core::error::ServerError;
use podkit_core::ids::{CronRunId, CronTaskId, ProjectId, TeamId};
use podkit_core::state::AppState;

const TASK: &str = "/teams/{team_id}/projects/{project_id}/cron/{task_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{TASK}{path}");
	Router::new()
		.route(&at("/run"), post(run_now))
		.route(&at("/runs"), get(runs_poll))
		.route(&at("/runs/{run_id}/cancel"), post(cancel))
		.route(&at("/runs/{run_id}/log"), get(log))
}

fn base(ctx: &TeamCtx, project: ProjectId, task: CronTaskId) -> String {
	super::task_url(ctx.team.id, project, task)
}

fn trigger_text(run: &CronRun) -> String {
	if run.attempt > 1 {
		format!("{} \u{b7} attempt {}", run.trigger.label(), run.attempt)
	} else {
		run.trigger.label().to_owned()
	}
}

/// The history card. `selected` is the run whose log is open.
pub(super) fn section(
	ctx: &TeamCtx,
	project: ProjectId,
	task: &CronTask,
	runs: &[CronRun],
	selected: Option<CronRunId>,
) -> Markup {
	let base = base(ctx, project, task.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Cron);
	let active = runs.iter().any(|r| r.status.is_active());
	let rows: Vec<Vec<Markup>> = runs
		.iter()
		.map(|run| {
			let cancel = format!(
				"@post({})",
				js_string(&format!("{base}/runs/{}/cancel", run.id.0))
			);
			vec![
				html! { span class="whitespace-nowrap" { (when(run.started_at.unwrap_or(run.created_at))) } },
				html! { (trigger_text(run)) },
				html! { span class="whitespace-nowrap" { (took(run)) } },
				html! {
					div class="ui-stat-cell" {
						(run_dot(run.status))
						@if let Some(code) = run.exit_code && run.status == RunStatus::Failed {
							p class="ui-hint" { "exit code " (code) }
						}
						@if let Some(message) = &run.error {
							p class="ui-cell-err" { (message) }
						}
					}
				},
				html! {
					div class="flex items-center gap-2" {
						@if run.job_id.is_some() {
							a class=(button_class(Variant::Outline, Size::Sm)) href={ (base) "?run=" (run.id.0) "#cron-log-box" }
								aria-current=[(selected == Some(run.id)).then_some("true")] { "Log" }
						}
						@if can_update && run.status.is_active() {
							button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(cancel) { "Cancel" }
						}
					}
				},
			]
		})
		.collect();
	let run_url = format!("@post({})", js_string(&format!("{base}/run")));
	let action = html! {
		@if can_update {
			button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(run_url)
				"data-indicator:_starting" "data-attr:disabled"="$_starting" { (icon("play")) "Run now" }
		}
	};
	html! {
		div id="cron-runs" {
			(card(&html! {
				(card_section("Runs", Some("The newest first. Old runs are removed after a while."), Some(&action)))
				@if runs.is_empty() {
					(card_content(&empty_state("clock", "No runs yet.", Some(if can_update { "The first one starts when the schedule comes up, or when you press Run now." } else { "The first one starts when the schedule comes up." }), None)))
				} @else {
					(card_content(&table("cron-run-table", "Runs of this task", &["Started", "Trigger", "Took", "Result", ""], &rows)))
				}
				(poll(&format!("{base}/runs?run={}", selected.map_or(0, |r| r.0)), active))
			}))
		}
	}
}

/// The log of one run, as the viewer; the stream is `runs/{id}/log`.
pub(super) fn log_card(
	ctx: &TeamCtx,
	project: ProjectId,
	task: &CronTask,
	run: &CronRun,
) -> Markup {
	let stream = format!("{}/runs/{}/log", base(ctx, project, task.id), run.id.0);
	let title = html! {
		span class="font-medium" { "Log of " (when(run.started_at.unwrap_or(run.created_at))) }
	};
	html! {
		div id="cron-log-box" {
			(log_viewer_titled("cron-log", "Run log", &stream, Some(&title)))
		}
	}
}

pub(super) async fn history(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	task: CronTaskId,
) -> Result<Vec<CronRun>, ServerError> {
	service::runs(&s.db, ctx.user.id, ctx.team.id, project, task, None, 30).await
}

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	task: CronTaskId,
	selected: Option<CronRunId>,
) -> Result<Markup, ServerError> {
	let view = service::get(&s.db, ctx.user.id, ctx.team.id, project, task).await?;
	let runs = history(s, ctx, project, task).await?;
	Ok(section(ctx, project, &view.task, &runs, selected))
}

#[derive(serde::Deserialize)]
struct PollQuery {
	run: Option<i64>,
}

async fn runs_poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
	axum::extract::Query(q): axum::extract::Query<PollQuery>,
) -> Response {
	let selected = q.run.filter(|r| *r > 0).map(CronRunId);
	match refreshed(&s, &ctx, project, task, selected).await {
		Ok(section) => sse::patch(&section).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn run_now(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> Response {
	let result = async {
		let run = service::run_now(&s, ctx.user.id, ctx.team.id, project, task).await?;
		refreshed(&s, &ctx, project, task, Some(run.id)).await
	}
	.await;
	match result {
		Ok(section) => sse::patch(&section)
			.and(sse::toast("Run queued"))
			.into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn cancel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task, run)): Path<(TeamId, ProjectId, CronTaskId, CronRunId)>,
) -> Response {
	let result = async {
		service::cancel(&s, ctx.user.id, ctx.team.id, project, task, run).await?;
		refreshed(&s, &ctx, project, task, Some(run)).await
	}
	.await;
	match result {
		Ok(section) => sse::patch(&section)
			.and(sse::toast("Cancelling the run"))
			.into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task, run)): Path<(TeamId, ProjectId, CronTaskId, CronRunId)>,
) -> Response {
	match service::run(&s.db, ctx.user.id, ctx.team.id, project, task, run).await {
		Ok(run) => job_log(&s, run.job_id, "cron-log"),
		Err(e) => {
			let status = e.status();
			if status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN {
				status.into_response()
			} else {
				error::action(&e)
			}
		}
	}
}
