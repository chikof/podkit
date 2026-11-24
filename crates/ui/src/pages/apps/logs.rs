//! The Logs tab: the running container's output, with a tail size and a follow switch.
//!
//! The controls are a real GET form, so the page works (and the choice survives a reload)
//! without script. The stream endpoint answers with `live::container_log`; a failure to start
//! (nothing running, the agent down) is written into the log itself rather than a 500.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::deployments_tab::stream_failure;
use super::{Tab, app_url, load, page};
use crate::components::{
	SELECT, Size, Variant, button_class, card, card_content, card_section, chip, chip_group,
	empty_state, log_viewer,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::live::container_log;
use crate::pages::logs::history::{self, Panel};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::deployments::service;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::state::AppState;

/// The tail lengths the Logs tab offers.
const TAILS: [u32; 3] = [200, 1000, 5000];

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/logs",
			get(logs_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/logs/stream",
			get(logs_stream),
		)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LogParams {
	tail: Option<u32>,
	follow: Option<String>,
	/// `history` shows the stored lines of every deploy instead of the running container.
	mode: Option<String>,
}

impl LogParams {
	fn tail(&self) -> u32 {
		self.tail.filter(|t| TAILS.contains(t)).unwrap_or(TAILS[0])
	}

	fn history(&self) -> bool {
		self.mode.as_deref() == Some("history")
	}

	/// Following is the default; only `follow=0` turns it off.
	fn follow(&self) -> bool {
		self.follow.as_deref() != Some("0")
	}
}

async fn logs_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(params): Query<LogParams>,
	Query(search): Query<history::Params>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let running = service::is_running(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let (tail, follow) = (params.tail(), params.follow());
	// The stored lines outlive the container, so history needs no running one.
	let can_history = ctx.perms.can(Action::Read, Resource::Logs);
	let history = can_history && params.history();
	let service = format!("app-{}", app.id);
	let mode = chip_group(
		"Show",
		&html! {
			(chip("Running container", &format!("{base}/logs"), !history, None))
			@if can_history {
				(chip("History across deploys", &format!("{base}/logs?mode=history"), history, None))
			}
		},
	);
	let content = card(&html! {
		(card_section(
			"Logs",
			Some(if history { "What this application printed, kept after each container is gone." } else { "Output of the running container." }),
			None,
		))
		(card_content(&html! {
			@if can_history { div class="mb-4" { (mode) } }
			@if history {
				(history::panel(
					&Panel {
						team: ctx.team.id,
						action: format!("{base}/logs"),
						keep: vec![("mode", "history")],
						fixed: Some(&service),
						sources: &[],
					},
					&search,
				))
			} @else if running {
				div class="flex flex-col gap-4" {
					form class="flex flex-wrap items-end gap-3" method="get" action={ (base) "/logs" } {
						label class="flex flex-col gap-2 text-sm leading-none font-medium sm:w-44" {
							"Lines"
							select class=(SELECT) name="tail" "data-on:change"="el.form.requestSubmit()" {
								button { selectedcontent {} }
								@for t in TAILS {
									option value=(t) selected[t == tail] { "Last " (t) }
								}
							}
						}
						label class="flex flex-col gap-2 text-sm leading-none font-medium sm:w-52" {
							"Follow"
							select class=(SELECT) name="follow" "data-on:change"="el.form.requestSubmit()" {
								button { selectedcontent {} }
								option value="1" selected[follow] { "Stream new lines" }
								option value="0" selected[!follow] { "Snapshot only" }
							}
						}
						noscript { button type="submit" class=(button_class(Variant::Outline, Size::Md)) { "Apply" } }
					}
					(log_viewer("runtime-log", "Runtime log", &format!("{base}/logs/stream?tail={tail}&follow={}", u8::from(follow))))
				}
			} @else {
				(empty_state(
					"scroll-text",
					"This application isn't running.",
					Some("Deploy it, or restart it from its last build, and its output shows up here."),
					None,
				))
			}
		}))
	});
	Ok(page(&s, &ctx, &project, &app, Tab::Logs, &content).await)
}

async fn logs_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(params): Query<LogParams>,
) -> Response {
	match service::runtime_log(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		app,
		params.tail(),
		params.follow(),
	)
	.await
	{
		Ok(events) => container_log(events, "runtime-log", s.shutdown.clone()),
		Err(e) => stream_failure(&e, "runtime-log"),
	}
}
