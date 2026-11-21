//! The project's scheduled tasks, and the card the Cron tab of an application shows.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::{new_url, task_table};
use crate::applications::service::ApplicationView;
use crate::auth::permission::{Action, Resource};
use crate::cron::Kind;
use crate::cron::service::{self, TaskView};
use crate::environments;
use crate::ids::{EnvironmentId, ProjectId, TeamId};
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	Size, Variant, button_class, card, card_content, card_section, empty_state, icon,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::pages::apps::project_url;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/cron",
		get(list_page),
	)
}

#[derive(Deserialize)]
struct ListQuery {
	environment: Option<EnvironmentId>,
}

fn crumbs<'a>(ctx: &TeamCtx, project: &'a projects::Project, last: &'a str) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		(last, None),
	]
}

/// The tasks of an application: what the Cron tab lists, with its one primary action.
pub fn tab_card(
	ctx: &TeamCtx,
	project: ProjectId,
	app: &ApplicationView,
	tasks: &[TaskView],
) -> Markup {
	let can_create = ctx.perms.can(Action::Create, Resource::Cron);
	let new = new_url(
		ctx.team.id,
		project,
		app.environment_id,
		Kind::Exec,
		Some(app.id),
	);
	let add = html! {
		@if can_create {
			a href=(new) class=(button_class(Variant::Outline, Size::Sm)) { (icon("plus")) "Add a task" }
		}
	};
	html! {
		div id="cron-tab" {
			(card(&html! {
				(card_section("Scheduled commands", Some("Commands run inside this app's container on a schedule, for example php artisan schedule:run."), Some(&add)))
				@if tasks.is_empty() {
					(card_content(&empty_state(
						"clock",
						"No scheduled commands.",
						Some(if can_create { "Add one to run a command in this app every hour, night or minute." } else { "Ask someone who can manage scheduled tasks to add one." }),
						None,
					)))
				} @else {
					(card_content(&task_table(ctx, project, "app-cron-table", "Scheduled commands of this application", tasks)))
				}
			}))
		}
	}
}

async fn list_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<ListQuery>,
) -> Result<Markup, Response> {
	let fail = |e| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let tasks = service::list(&s.db, ctx.user.id, ctx.team.id, project.id, None, None)
		.await
		.map_err(fail)?;
	let can_create = ctx.perms.can(Action::Create, Resource::Cron);
	// New tasks go in the environment asked for, else the project's first.
	let env = match q.environment {
		Some(id) => Some(id),
		None => environments::service::list(&s.db, ctx.user.id, ctx.team.id, project.id)
			.await
			.map_err(fail)?
			.first()
			.map(|e| e.id),
	};
	let actions = html! {
		@if can_create && let Some(env) = env {
			a href=(new_url(ctx.team.id, project.id, env, Kind::Container, None)) class=(button_class(Variant::Outline, Size::Md)) {
				(icon("box")) "One-shot container"
			}
			a href=(new_url(ctx.team.id, project.id, env, Kind::Exec, None)) class=(button_class(Variant::Default, Size::Md)) {
				(icon("plus")) "Command in an app"
			}
		}
	};
	let body = html! {
		div class="ui-wide ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { (project.name) }
					h1 { "Scheduled tasks" }
					p class="ui-lede" { "Commands that run on a cron schedule, in an app or in a one-shot container." }
				}
				div class="ui-page-actions" { (actions) }
			}
			@if tasks.is_empty() {
				(empty_state(
					"clock",
					"No scheduled tasks yet.",
					Some(if can_create { "Run php artisan schedule:run every minute, clean up every night, or send a report on Mondays." } else { "Ask someone who can manage scheduled tasks to add one." }),
					None,
				))
			} @else {
				(card(&card_content(&task_table(&ctx, project.id, "cron-table", "Scheduled tasks of this project", &tasks))))
			}
		}
	};
	Ok(app_page(
		&ctx,
		"Scheduled tasks",
		&crumbs(&ctx, &project, "Scheduled tasks"),
		&body,
	))
}
