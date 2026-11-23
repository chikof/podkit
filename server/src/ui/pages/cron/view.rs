//! A task's page: what it runs and when, Pause and Delete, the settings behind a save bar,
//! the run history and the log of the selected run.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::form::{self, FIELDS, Signals};
use super::{cron_url, runs, target};
use crate::auth::permission::{Action, Resource};
use crate::cron::service::{self, TaskView};
use crate::ids::{CronRunId, CronTaskId, ProjectId, TeamId};
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, Size, Variant, button_class, card, card_content, card_section, dialog, icon,
	save_bar, save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::pages::apps::project_url;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	let task = "/teams/{team_id}/projects/{project_id}/cron/{task_id}";
	Router::new()
		.route(task, get(page).post(save))
		.route(&format!("{task}/pause"), post(pause))
		.route(&format!("{task}/resume"), post(resume))
		.route(&format!("{task}/delete"), post(delete))
}

#[derive(Deserialize)]
struct PageQuery {
	run: Option<i64>,
}

fn facts(view: &TaskView) -> Markup {
	let t = &view.task;
	html! {
		dl class="ui-facts ui-facts-col" {
			div { dt { "Runs in" } dd { (target(view)) } }
			div { dt { "Command" } dd { code { (t.command) } } }
			div { dt { "Schedule" } dd { code { (t.schedule) } " " (t.timezone) } }
			div { dt { "Timeout" } dd { (service::duration(t.timeout_s.unsigned_abs().into())) } }
			div { dt { "Overlapping runs" } dd { (t.concurrency.label()) } }
			div { dt { "Retries" } dd { (t.max_retries) @if t.max_retries > 0 { ", first after " (service::duration(t.retry_backoff_s.unsigned_abs().into())) } } }
		}
	}
}

fn delete_dialog(ctx: &TeamCtx, project: ProjectId, view: &TaskView) -> Markup {
	let action = format!(
		"@post({})",
		js_string(&format!(
			"{}/delete",
			super::task_url(ctx.team.id, project, view.task.id)
		))
	);
	dialog(
		"delete-cron-dialog",
		"Delete this task?",
		Some("A run that is going is cancelled. The history and its logs go with it."),
		&html! {
			p { "Delete " strong class="break-all" { (view.task.name) } "?" }
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(action)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete task" }
		},
	)
}

/// The page body for a task; `selected` is the run whose log is shown.
async fn body(
	s: &AppState,
	ctx: &TeamCtx,
	project: &projects::Project,
	view: &TaskView,
	selected: Option<CronRunId>,
) -> Result<Markup, crate::error::ServerError> {
	let task = &view.task;
	let history = runs::history(s, ctx, project.id, task.id).await?;
	let chosen = history
		.iter()
		.find(|r| Some(r.id) == selected && r.job_id.is_some())
		.or_else(|| history.iter().find(|r| r.job_id.is_some()));
	let can_update = ctx.perms.can(Action::Update, Resource::Cron);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Cron);
	let base = super::task_url(ctx.team.id, project.id, task.id);
	let toggle = format!(
		"@post({})",
		js_string(&format!(
			"{base}/{}",
			if task.enabled { "pause" } else { "resume" }
		))
	);
	let signals = form::seed(
		Some(task),
		task.kind,
		task.environment_id,
		task.application_id,
	)
	.to_string();
	let runs_preview = form::upcoming(&task.schedule, &task.timezone);
	let kind = task.kind;
	let settings = if can_update {
		let choices = form::choices(s, ctx, project.id, task.environment_id).await?;
		let save = format!("evt.preventDefault(); @post({})", js_string(&base));
		html! {
			form id="cron-settings" "data-on:submit"=(save) aria-label="Task settings" {
				(card(&html! {
					(card_section("Settings", Some("Saved changes apply from the next run."), None))
					(card_content(&form::fields(&form::Layout { ctx, project: project.id, kind, locked_app: true, choices: &choices, runs: &runs_preview, timezone: &task.timezone, editing: true })))
					(save_bar(&FIELDS))
				}))
			}
		}
	} else {
		card(&html! {
			(card_section("Settings", None, None))
			(card_content(&facts(view)))
			(card_content(&form::hint(&runs_preview, &task.timezone)))
		})
	};
	Ok(html! {
		div class="ui-wide ui-page-body" "data-signals"=(signals) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { (project.name) " \u{b7} " (kind.label()) }
					h1 { (task.name) }
					p class="ui-lede" { code { (task.command) } }
				}
				div class="ui-page-actions" {
					@if can_update {
						button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(toggle) {
							@if task.enabled { (icon("pause")) "Pause" } @else { (icon("play")) "Resume" }
						}
					}
					@if can_delete {
						button type="button" class=(button_class(Variant::Outline, Size::Md)) command="show-modal"
							commandfor="delete-cron-dialog" aria-haspopup="dialog" { (icon("trash-2")) "Delete" }
					}
				}
			}
			(runs::section(ctx, project.id, task, &history, chosen.map(|r| r.id)))
			@if let Some(run) = chosen { (runs::log_card(ctx, project.id, task, run)) }
			(settings)
			@if can_delete { (delete_dialog(ctx, project.id, view)) }
		}
	})
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
	Query(q): Query<PageQuery>,
) -> Result<Markup, Response> {
	let fail = |e| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let view = service::get(&s.db, ctx.user.id, ctx.team.id, project.id, task)
		.await
		.map_err(fail)?;
	let selected = q.run.filter(|r| *r > 0).map(CronRunId);
	let content = body(&s, &ctx, &project, &view, selected)
		.await
		.map_err(fail)?;
	let crumbs: Vec<Crumb<'_>> = vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		("Scheduled tasks", Some(cron_url(ctx.team.id, project.id))),
		(view.task.name.as_str(), None),
	];
	Ok(app_page(&ctx, &view.task.name, &crumbs, &content))
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		let current = service::get(&s.db, ctx.user.id, ctx.team.id, project, task).await?;
		let input = sig.update_input(&current.task)?;
		service::update(&s.db, ctx.user.id, ctx.team.id, project, task, input).await
	}
	.await;
	match result {
		Ok(saved) => {
			let runs = form::upcoming(&saved.schedule, &saved.timezone);
			sse::patch(&form::hint(&runs, &saved.timezone))
				.and(sse::signals(&json!({ "_error": "" })))
				.and(sse::toast("Task saved"))
				.and(save_bar_saved())
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn set_enabled(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	task: CronTaskId,
	on: bool,
) -> Response {
	match service::set_enabled(&s.db, ctx.user.id, ctx.team.id, project, task, on).await {
		Ok(_) => sse::redirect(&super::task_url(ctx.team.id, project, task)).into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn pause(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> Response {
	set_enabled(&s, &ctx, project, task, false).await
}

async fn resume(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> Response {
	set_enabled(&s, &ctx, project, task, true).await
}

async fn delete(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, task)): Path<(TeamId, ProjectId, CronTaskId)>,
) -> Response {
	match service::remove(&s, ctx.user.id, ctx.team.id, project, task).await {
		Ok(_) => sse::redirect(&cron_url(ctx.team.id, project)).into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}
