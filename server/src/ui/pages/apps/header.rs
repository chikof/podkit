//! The head of an application's page: logo, name, type, status, public URL, and the actions
//! Deploy, Restart, Stop and a menu with Rollback and Delete. Delete and Rollback open
//! dialogs; deleting asks for the slug to be typed.
//!
//! Every action comes back as an SSE stream. A refusal ("not running", "no previous build")
//! lands in the `_action_error` signal, which the header shows, so no tab needs its own line.
//! While the newest deployment is in flight the header polls itself.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde_json::json;

use super::{AppPath, app_url, public_link};
use crate::applications::SourceKind;
use crate::applications::service::{self, ApplicationView};
use crate::auth::permission::{Action, Resource};
use crate::deployments::DeploymentStatus;
use crate::deployments::service::{self as deployments, DeploymentView};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::projects::overview::{ResourceStatus, application_mark};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, Dismiss, INPUT, LABEL, LogoSize, MENU_ITEM,
	MENU_ITEM_DANGER, Size, Tone, Variant, badge, button_class, dialog, dropdown_menu, icon, logo,
	menu_label, menu_separator,
};
use crate::ui::extract::TeamCtx;
use crate::ui::pages::projects::cards::status_line;
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll};
use crate::ui::{error, sse};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{APP}{path}");
	Router::new()
		.route(&at("/header"), get(fragment))
		.route(&at("/deploy"), post(deploy))
		.route(&at("/restart"), post(restart))
		.route(&at("/stop"), post(stop))
}

/// A signal update that shows `msg` in the header's action error line.
pub(super) fn action_error(msg: &str) -> sse::Events {
	sse::signals(&json!({ "_action_error": msg }))
}

pub(super) fn fail(e: &ServerError) -> Response {
	action_error(&error::message(e)).into_response()
}

pub(super) fn deployment_url(base: &str, id: crate::ids::DeploymentId) -> String {
	format!("{base}/deployments/{id}")
}

fn status_of(app: &ApplicationView) -> ResourceStatus {
	app.latest
		.map_or(ResourceStatus::NotDeployed, ResourceStatus::Deployment)
}

// --- the header ---------------------------------------------------------------------------

/// Deploy, Restart and Stop, each shown only to someone who may use it and enabled only
/// while it can work. Refusals the server still gives (a stale page, say) show in the
/// error line. Restart and Stop fold into the menu on a narrow screen.
fn actions(ctx: &TeamCtx, app: &ApplicationView, rollback_to: Option<&DeploymentView>) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Deployment);
	let can_stop = ctx.perms.can(Action::Update, Resource::Deployment);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Application);
	let busy = app.latest.is_some_and(Status::in_flight);
	let running = app.latest == Some(DeploymentStatus::Running);
	let can_restart = can_create && !busy && app.latest.is_some();
	let post = |action: &str| format!("@post({})", js_string(&format!("{base}/{action}")));
	let menu_class = button_class(Variant::Outline, Size::Icon);
	let open = |id: &str| {
		format!(
			"el.commandForElement || document.getElementById({}).showModal()",
			js_string(id)
		)
	};
	let has_menu = can_create || can_stop || can_delete;
	html! {
		div class="ui-app-actions" {
			@if can_create {
				button type="button" class=(BUTTON) "data-on:click"=(post("deploy"))
					"data-indicator:_acting" "data-attr:disabled"="$_acting" {
					(icon("rocket")) "Deploy"
				}
				@if can_restart {
					button type="button" class={ (BUTTON_OUTLINE) " ui-app-secondary" } "data-on:click"=(post("restart"))
						"data-attr:disabled"="$_acting" { (icon("refresh-cw")) "Restart" }
				} @else {
					button type="button" class={ (BUTTON_OUTLINE) " ui-app-secondary" } disabled { (icon("refresh-cw")) "Restart" }
				}
			}
			@if can_stop {
				@if running {
					button type="button" class={ (BUTTON_DANGER) " ui-app-secondary" } "data-on:click"=(post("stop"))
						"data-attr:disabled"="$_acting" { (icon("square")) "Stop" }
				} @else {
					button type="button" class={ (BUTTON_DANGER) " ui-app-secondary" } disabled { (icon("square")) "Stop" }
				}
			}
			@if has_menu {
				(dropdown_menu(
					"app-menu",
					&menu_class,
					&format!("Actions for {}", app.name),
					&icon("ellipsis"),
					&html! {
						(menu_label("Application"))
						@if can_create {
							@if can_restart {
								button type="button" class={ (MENU_ITEM) " ui-app-collapsed" } "data-on:click"=(post("restart")) { (icon("refresh-cw")) "Restart" }
							}
						}
						@if can_stop && running {
							button type="button" class={ (MENU_ITEM_DANGER) " ui-app-collapsed" } "data-on:click"=(post("stop")) { (icon("square")) "Stop" }
						}
						@if can_create {
							@if let Some(target) = rollback_to {
								button type="button" class=(MENU_ITEM) command="show-modal" commandfor="rollback-dialog"
									"data-on:click"={ "$_action_error = ''; $_rb = '" (target.id) "'; " (open("rollback-dialog")) } {
									(icon("undo-2")) "Rollback"
								}
							} @else {
								button type="button" class=(MENU_ITEM) disabled { (icon("undo-2")) "Rollback" }
							}
						}
						@if can_delete {
							(menu_separator())
							button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor="delete-dialog"
								"data-on:click"={ "$confirm = ''; $_delete_error = ''; " (open("delete-dialog")) } {
								(icon("trash-2")) "Delete"
							}
						}
					},
				))
			}
		}
	}
}

/// The type of the source, in words.
fn source_label(app: &ApplicationView) -> &'static str {
	match app.source_kind {
		SourceKind::Git => "Git",
		SourceKind::Image => "Docker image",
	}
}

/// What the application is built from, as one line.
pub(super) fn source_line(app: &ApplicationView) -> String {
	match (&app.image, &app.repo_url) {
		(Some(image), _) => image.clone(),
		(None, Some(url)) => format!("{url}@{}", app.git_ref),
		(None, None) => String::new(),
	}
}

/// `#app-header`: what polling and the actions re-render. `rows` are the newest deployments
/// (newest first), for the Rollback entry.
pub fn header(
	s: &AppState,
	ctx: &TeamCtx,
	app: &ApplicationView,
	rows: &[DeploymentView],
) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let link = public_link(s, &app.url);
	let busy = app.latest.is_some_and(Status::in_flight);
	let mark = application_mark(app.repo_url.as_deref(), app.build_strategy);
	let rollback_to = rows.iter().find(|d| d.can_rollback());
	html! {
		div id="app-header" class="flex flex-col gap-3" {
			(poll(&format!("{base}/header"), busy))
			div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4" {
				div class="flex min-w-0 items-center gap-4" {
					(logo(mark.unwrap_or(&app.name), LogoSize::Lg))
					div class="flex min-w-0 flex-col gap-1.5" {
						div class="flex flex-wrap items-center gap-x-3 gap-y-1" {
							h1 class="min-w-0 truncate text-2xl font-semibold tracking-tight" { (app.name) }
							(badge(Tone::Neutral, "Application"))
							(badge(Tone::Neutral, source_label(app)))
						}
						div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm" {
							(status_line(status_of(app)))
							a href=(link) class="inline-flex min-w-0 items-center gap-1 text-muted-foreground hover:text-foreground hover:underline"
								target="_blank" rel="noopener noreferrer" {
								span class="truncate" { (link) }
								(icon("external-link"))
							}
						}
						p class="font-mono text-xs break-all text-muted-foreground" { (source_line(app)) }
					}
				}
				(actions(ctx, app, rollback_to))
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
		}
	}
}

// --- dialogs --------------------------------------------------------------------------------

/// Rollback and Delete, opened from the menu (and Rollback from a deployment's row). They sit
/// outside the header so a poll never closes them.
pub fn dialogs(ctx: &TeamCtx, app: &ApplicationView) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Deployment);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Application);
	let rollback = format!(
		"@post({} + $_rb + {})",
		js_string(&format!("{base}/deployments/")),
		js_string("/rollback")
	);
	let delete = format!("@post({})", js_string(&format!("{base}/delete")));
	let matches = format!("$confirm !== {}", js_string(&app.slug));
	html! {
		@if can_create {
			(dialog(
				"rollback-dialog",
				"Roll back to an earlier build",
				Some("Nothing is rebuilt: podkit starts the image of that deployment and replaces the running container."),
				Dismiss::Esc,
				&html! {
					div class="flex flex-col gap-3" {
						p { "Roll back to deployment " strong class="font-mono" { "#" span "data-text"="$_rb" {} } "?" }
						p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
					}
				},
				&html! {
					form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep the current build" } }
					button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(rollback)
						"data-indicator:_acting" "data-attr:disabled"="$_acting" { "Roll back" }
				},
			))
		}
		@if can_delete {
			(dialog(
				"delete-dialog",
				"Delete this application?",
				Some("Deleting stops and removes the application's containers and volumes. This can't be undone."),
				Dismiss::Esc,
				&html! {
					div class="flex flex-col gap-3" {
						label class=(LABEL) {
							span class="leading-normal" { "Type " strong class="font-mono" { (app.slug) } " to confirm" }
							input class=(INPUT) type="text" autocomplete="off" spellcheck="false" autocapitalize="off"
								aria-label="Application slug" "data-bind:confirm";
						}
						p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
					}
				},
				&html! {
					form method="dialog" {
						button class=(button_class(Variant::Outline, Size::Md)) autofocus
							"data-on:click"="$confirm = ''; $_delete_error = ''" { "Cancel" }
					}
					button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(delete)
						"data-indicator:_deleting" "data-attr:disabled"={ "$_deleting || " (matches) } disabled { "Delete application" }
				},
			))
		}
	}
}

// --- actions --------------------------------------------------------------------------------

/// What polling fetches: the header again, which drops its own poller once nothing is in
/// flight. A lost or foreign application is a real 404, so the poll stops.
async fn fragment(State(s): State<AppState>, ctx: TeamCtx, path: AppPath) -> Response {
	let Path((_, project, app)) = path;
	match service::get(&s.db, ctx.user.id, ctx.team.id, project, app).await {
		Ok(view) => {
			let rows = super::recent_deployments(&s, &ctx, &view).await;
			sse::patch(&header(&s, &ctx, &view, &rows)).into_response()
		}
		Err(e) => error::page(&ctx, &e),
	}
}

async fn deploy(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match deployments::deploy(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(d) => sse::redirect(&deployment_url(&app_url(ctx.team.id, project, app), d.id))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn restart(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match deployments::restart(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(d) => sse::redirect(&deployment_url(&app_url(ctx.team.id, project, app), d.id))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn stop(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	// The header, the history and the status all change: start over.
	match deployments::stop(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(_) => sse::reload().into_response(),
		Err(e) => fail(&e),
	}
}
