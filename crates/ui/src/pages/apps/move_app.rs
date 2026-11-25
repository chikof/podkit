//! Move to another server (`moves`): the header's menu opens a dialog and asks for its body,
//! which says, for each other server of the team, whether the application can go there,
//! how (the method), what its users notice (the downtime) and what the owner has to do
//! afterwards. Each server it can go to has its own button. An application with volumes
//! needs a backup destination for the copy, picked at the top.
//!
//! While a move is queued or running the body shows it, step by step, and asks for itself
//! again every few seconds.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;

use super::app_url;
use super::header::fail;
use crate::components::{SELECT, Size, Tone, Variant, badge, button_class, dialog, icon};
use crate::extract::TeamCtx;
use crate::sse;
use crate::sse::js_string;
use crate::status::{poll, status_badge};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{ApplicationId, BackupTargetId, ProjectId, ServerId, TeamId};
use podkit_core::moves::service::{self, Candidate, Plan};
use podkit_core::moves::{Move, MoveStatus};
use podkit_core::state::AppState;

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

/// The id of the dialog, and of the element its body is patched into.
pub const DIALOG: &str = "move-dialog";
const BODY: &str = "move-body";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{APP}{path}");
	Router::new()
		.route(&at("/move"), get(body))
		.route(&at("/move/{server_id}"), post(run))
}

/// Whether the person may move applications: `update application` and `read server`.
#[must_use]
pub fn allowed(ctx: &TeamCtx) -> bool {
	ctx.perms.can(Action::Update, Resource::Application)
		&& ctx.perms.can(Action::Read, Resource::Server)
}

/// What the menu entry does: clear the error line, ask for a fresh body and open the dialog.
#[must_use]
pub fn open_action(base: &str) -> String {
	format!(
		"$_action_error = ''; @get({}); el.commandForElement || document.getElementById({}).showModal()",
		js_string(&format!("{base}/move")),
		js_string(DIALOG)
	)
}

/// The dialog, with a placeholder body until the menu entry asks for the real one.
pub fn dialog_for(ctx: &TeamCtx) -> Markup {
	if !allowed(ctx) {
		return html! {};
	}
	dialog(
		DIALOG,
		"Move to another server",
		Some(
			"The application keeps its settings, variables, domains and history; only the server it runs on changes.",
		),
		&html! {
			div id=(BODY) class="flex flex-col gap-3" data-signals="{move_backup: ''}" {
				p class="text-sm text-muted-foreground" { "Loading\u{2026}" }
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
		},
		&html! {
			form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Close" } }
		},
	)
}

fn move_tone(status: MoveStatus) -> Tone {
	match status {
		MoveStatus::Queued | MoveStatus::Running => Tone::Progress,
		MoveStatus::Succeeded => Tone::Success,
		MoveStatus::Failed => Tone::Danger,
		MoveStatus::Cancelled => Tone::Neutral,
	}
}

/// One move, in a line: where to, how it went, the step it is on or ended with.
fn move_line(m: &Move) -> Markup {
	html! {
		li class="flex flex-col gap-0.5" {
			span class="text-sm" {
				(m.from_server_name) " \u{2192} " (m.to_server_name) " "
				(badge(move_tone(m.status), m.status.as_str()))
			}
			span class="text-xs text-muted-foreground" { (m.method.label()) ": " (m.step) }
			@if let Some(error) = &m.error {
				span class="text-xs text-destructive" { (error) }
			}
		}
	}
}

/// The button of one server: posts the move, with the destination when there are volumes.
fn move_button(base: &str, plan: &Plan, c: &Candidate) -> Markup {
	let post = format!(
		"@post({})",
		js_string(&format!("{base}/move/{}", c.server.id))
	);
	let disabled = if plan.volumes > 0 {
		"$_acting || $move_backup === ''"
	} else {
		"$_acting"
	};
	html! {
		button type="button" class=(button_class(Variant::Default, Size::Sm)) "data-on:click"=(post)
			"data-indicator:_acting" "data-attr:disabled"=(disabled) {
			(icon("arrow-right")) "Move to " (c.server.name)
		}
	}
}

/// One server the application could go to.
fn candidate_row(base: &str, plan: &Plan, c: &Candidate) -> Markup {
	let v = &c.verdict;
	html! {
		li class="flex flex-col gap-2 rounded-[length:var(--r-md)] border border-border px-4 py-3" {
			div class="flex flex-wrap items-center justify-between gap-3" {
				div class="flex min-w-0 items-center gap-2" {
					span class="text-sm font-medium" { (c.server.name) }
					(status_badge(c.server.status))
					@if c.draining { (badge(Tone::Warning, "Draining")) }
				}
				@if v.movable() { (move_button(base, plan, c)) }
			}
			@if v.movable() {
				dl class="ui-facts" {
					div class="ui-fact" { dt { "Method" } dd { (v.method.label()) } }
					div class="ui-fact" { dt { "Downtime" } dd { (v.downtime) } }
				}
				@if !v.warnings.is_empty() {
					ul class="flex flex-col gap-1" aria-label={ "What changes on " (c.server.name) } {
						@for w in &v.warnings {
							li class="text-xs text-muted-foreground" { (w) }
						}
					}
				}
			} @else {
				ul class="flex flex-col gap-1" aria-label={ "Why not " (c.server.name) } {
					@for r in &v.refusals {
						li class="text-xs text-destructive" { (r) }
					}
				}
			}
		}
	}
}

fn body_markup(ctx: &TeamCtx, plan: &Plan) -> Markup {
	let app = &plan.application;
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let active = plan.active();
	html! {
		div id=(BODY) class="flex flex-col gap-4" {
			dl class="ui-facts ui-facts-row" aria-label="Where it runs" {
				div { dt { "Runs on" } dd { (plan.server.as_ref().map_or("an unknown server", |s| s.name.as_str())) } }
				div { dt { "Volumes" } dd { (plan.volumes) } }
			}
			@if let Some(m) = active {
				div class="flex flex-col gap-1 rounded-[length:var(--r-md)] border border-border px-4 py-3" role="status" {
					span class="text-sm font-medium" { "Moving to " (m.to_server_name) }
					span class="text-xs text-muted-foreground" { (m.step) }
				}
				(poll(&format!("{base}/move"), true))
			}
			@if plan.volumes > 0 && active.is_none() {
				label class="flex flex-col gap-2 text-sm" {
					"Copy the volume data through"
					@if plan.destinations.is_empty() {
						span class="text-xs text-destructive" {
							"No backup destination you can use. Add an S3 destination in Settings, Backups first."
						}
					} @else {
						select class=(SELECT) "data-bind:move_backup" {
							button { selectedcontent {} }
							option value="" { "Pick a destination" }
							@for d in &plan.destinations {
								option value=(d.id.0) { (d.name) }
							}
						}
						span class="ui-hint" { "The application is stopped, each volume archived there and restored on the new server, and it is started there." }
					}
				}
			}
			@if active.is_some() {
				// Nothing else can start until it ends.
			} @else if plan.candidates.is_empty() {
				p class="text-sm text-muted-foreground" {
					"This team has only one server. Add another to move applications between them."
				}
			} @else {
				ul class="flex flex-col gap-2" aria-label="Servers to move to" {
					@for c in &plan.candidates {
						(candidate_row(&base, plan, c))
					}
				}
			}
			@let past: Vec<&Move> = plan.history.iter().filter(|m| !m.status.in_flight()).take(3).collect();
			@if !past.is_empty() {
				div class="flex flex-col gap-2" {
					span class="text-xs text-muted-foreground" { "Earlier moves" }
					ul class="flex flex-col gap-2" { @for m in past { (move_line(m)) } }
				}
			}
		}
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Signals {
	move_backup: String,
}

impl Signals {
	fn backup(&self) -> Option<BackupTargetId> {
		self.move_backup.trim().parse().ok().map(BackupTargetId)
	}
}

/// The dialog's body, asked for each time it opens (and while a move runs).
async fn body(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	// Judged as if a destination will be picked: the button waits for one.
	match service::plan(&s.db, ctx.user.id, ctx.team.id, project, app, true).await {
		Ok(plan) => sse::patch(&body_markup(&ctx, &plan))
			.and(super::header::action_error(""))
			.into_response(),
		Err(e) => fail(&e),
	}
}

/// Starts the move, then shows it in the dialog.
async fn run(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, server)): Path<(TeamId, ProjectId, ApplicationId, ServerId)>,
	signals: Option<ReadSignals<Signals>>,
) -> Response {
	let backup = signals.and_then(|ReadSignals(s)| s.backup());
	let started = service::start(&s, ctx.user.id, ctx.team.id, project, app, server, backup).await;
	if let Err(e) = started {
		return fail(&e);
	}
	match service::plan(&s.db, ctx.user.id, ctx.team.id, project, app, true).await {
		Ok(plan) => sse::patch(&body_markup(&ctx, &plan))
			.and(super::header::action_error(""))
			.into_response(),
		Err(e) => fail(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_menu_entry_asks_for_the_body_and_opens_the_dialog() {
		let js = open_action("/teams/1/projects/2/apps/3");
		assert!(
			js.contains("@get('/teams/1/projects/2/apps/3/move')"),
			"{js}"
		);
		assert!(js.contains("move-dialog"), "{js}");
	}

	#[test]
	fn the_destination_is_read_from_the_signal() {
		let s = Signals {
			move_backup: "42".to_owned(),
		};
		assert_eq!(s.backup(), Some(BackupTargetId(42)));
		assert_eq!(Signals::default().backup(), None);
	}
}
