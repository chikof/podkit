//! Scheduled tasks: the project's list, a task's page (settings, next runs, run history with
//! the log), the create form, and the Cron tab of an application (its exec tasks).
//!
//! A task is a command on a cron schedule, run inside an app's container (`exec`) or in a
//! one-shot container. Everything a person does goes through `crate::cron::service`, which
//! authorizes and scopes it; these pages only render and post.

use std::fmt::Write as _;

use axum::Router;
use maud::{Markup, html};

use crate::cron::service::TaskView;
use crate::cron::{CronRun, Kind, RunStatus, schedule};
use crate::ids::{ApplicationId, CronTaskId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{Status as Shape, table};
use crate::ui::extract::TeamCtx;
use crate::ui::pages::apps::chrome;

mod form;
mod list;
mod runs;
#[cfg(test)]
mod tests;
mod view;

pub use list::tab_card;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(list::router())
		.merge(form::router())
		.merge(view::router())
		.merge(runs::router())
}

pub fn cron_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}/cron")
}

pub fn task_url(team: TeamId, project: ProjectId, task: CronTaskId) -> String {
	format!("{}/{task}", cron_url(team, project))
}

/// The create form: `kind` and the environment are fixed by where the person came from.
pub fn new_url(
	team: TeamId,
	project: ProjectId,
	environment: crate::ids::EnvironmentId,
	kind: Kind,
	application: Option<ApplicationId>,
) -> String {
	let mut url = format!(
		"{}/new?environment={environment}&kind={}",
		cron_url(team, project),
		kind.as_str()
	);
	if let Some(app) = application {
		let _ = write!(url, "&application={app}");
	}
	url
}

/// A run's state as Quiet's shaped dot and the word.
const fn shape(status: RunStatus) -> Shape {
	match status {
		RunStatus::Queued => Shape::Pending,
		RunStatus::Running => Shape::Building,
		RunStatus::Succeeded => Shape::Running,
		RunStatus::Failed => Shape::Failed,
		RunStatus::Cancelled | RunStatus::Skipped => Shape::Stopped,
	}
}

pub(super) fn run_dot(status: RunStatus) -> Markup {
	chrome::dot(shape(status), status.label())
}

/// The state of a task's newest run, or why there is none.
fn last_cell(task: &TaskView) -> Markup {
	html! {
		@if !task.task.enabled {
			span class="ui-badge" data-status="stopped" {
				span class="ui-dot" aria-hidden="true" {}
				span { "Paused" }
			}
		} @else if let Some(run) = &task.last_run {
			(run_dot(run.status))
		} @else {
			span class="text-muted-foreground" { "Never run" }
		}
	}
}

fn next_cell(task: &TaskView) -> String {
	match (task.task.enabled, task.task.next_run_at) {
		(true, Some(t)) => schedule::local_text(t, &task.task.timezone),
		(false, _) => "Paused".to_owned(),
		(true, None) => "\u{2014}".to_owned(),
	}
}

/// What a task runs in, for a row.
fn target(task: &TaskView) -> Markup {
	html! {
		@match (task.task.kind, &task.application, &task.task.image) {
			(Kind::Exec, Some(app), _) => { "in " strong { (app) } },
			(Kind::Exec, None, _) => "in its app",
			(Kind::Container, Some(app), Some(image)) => { span class="ui-mono" { (image) } " with " strong { (app) } "'s settings" },
			(Kind::Container, Some(app), None) => { "image of " strong { (app) } },
			(Kind::Container, None, Some(image)) => span class="ui-mono" { (image) },
			(Kind::Container, None, None) => "one-shot container",
		}
	}
}

/// The list of tasks as a table, shared by the project page and an application's tab.
pub(super) fn task_table(
	ctx: &TeamCtx,
	project: ProjectId,
	id: &str,
	caption: &str,
	tasks: &[TaskView],
) -> Markup {
	let rows: Vec<Vec<Markup>> = tasks
		.iter()
		.map(|t| {
			let href = task_url(ctx.team.id, project, t.task.id);
			vec![
				html! {
					a class="ui-link font-medium" href=(href) { (t.task.name) }
					p class="ui-hint" { (target(t)) }
				},
				html! {
					code { (t.task.schedule) }
					p class="ui-hint" { (t.task.timezone) }
				},
				html! { span class="whitespace-nowrap" { (next_cell(t)) } },
				last_cell(t),
			]
		})
		.collect();
	table(
		id,
		caption,
		&["Task", "Schedule", "Next run", "Last run"],
		&rows,
	)
}

/// `1.2 s`, `3 min 4 s`: how long a run took, from its start to its end.
pub(super) fn took(run: &CronRun) -> String {
	let (Some(start), Some(end)) = (run.started_at, run.finished_at) else {
		return "\u{2014}".to_owned();
	};
	duration_text((end - start).whole_seconds().max(0))
}

pub(super) fn duration_text(secs: i64) -> String {
	match secs {
		s if s < 60 => format!("{s} s"),
		s if s < 3600 => format!("{} min {} s", s / 60, s % 60),
		s => format!("{} h {} min", s / 3600, (s % 3600) / 60),
	}
}
