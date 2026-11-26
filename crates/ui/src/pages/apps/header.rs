//! The head of an application's page: logo, name, type, status, public URL, and the actions
//! Deploy, Restart, Stop and a menu with Rollback, Promote, Move to another server and Delete.
//! Delete, Rollback, Promote and Move open dialogs; deleting asks for the slug to be typed.
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

use super::{AppPath, app_url, chrome, public_link};
use crate::components::{
	BUTTON, BUTTON_OUTLINE, INPUT, LABEL, LogoSize, MENU_ITEM, MENU_ITEM_DANGER, Size,
	Status as Shape, Variant, button_class, dialog, dropdown_menu, icon, logo, menu_label,
	menu_separator,
};
use crate::extract::TeamCtx;
use crate::pages::security;
use crate::sse::js_string;
use crate::status::{Status, poll};
use crate::{error, sse};
use podkit_core::applications::SourceKind;
use podkit_core::applications::service::{self, ApplicationView};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::deployments::DeploymentStatus;
use podkit_core::deployments::service::{self as deployments, DeploymentView};
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::metrics::service as metrics;
use podkit_core::metrics::{Range, ResourcePoint};
use podkit_core::projects::overview::application_mark;
use podkit_core::scanning::Subject;
use podkit_core::state::AppState;
use time::OffsetDateTime;

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

pub(super) fn deployment_url(base: &str, id: podkit_core::ids::DeploymentId) -> String {
	format!("{base}/deployments/{id}")
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
	let can_move = super::move_app::allowed(ctx);
	let has_menu = can_create || can_stop || can_delete || can_move;
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
					button type="button" class={ (BUTTON_OUTLINE) " ui-app-secondary" } "data-on:click"=(post("stop"))
						"data-attr:disabled"="$_acting" { (icon("square")) "Stop" }
				} @else {
					button type="button" class={ (BUTTON_OUTLINE) " ui-app-secondary" } disabled { (icon("square")) "Stop" }
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
							// Only a running release has an image to promote.
							@if running {
								button type="button" class=(MENU_ITEM) command="show-modal" commandfor=(super::promote::DIALOG)
									"data-on:click"=(super::promote::open_action(&base)) {
									(icon("arrow-up-right")) "Promote"
								}
							} @else {
								button type="button" class=(MENU_ITEM) disabled { (icon("arrow-up-right")) "Promote" }
							}
						}
						@if can_move {
							button type="button" class=(MENU_ITEM) command="show-modal" commandfor=(super::move_app::DIALOG)
								"data-on:click"=(super::move_app::open_action(&base)) {
								(icon("server")) "Move to another server"
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

/// The kind of the application, in words.
fn source_label(app: &ApplicationView) -> &'static str {
	match app.source_kind {
		SourceKind::Git => "Git app",
		SourceKind::Image => "Docker image app",
	}
}

/// The newest CPU and memory reading, for the figures under the head. Without one (nothing
/// has run yet, or the sampler has not reached it) they read as a dash.
async fn usage(s: &AppState, ctx: &TeamCtx, app: &ApplicationView) -> Option<ResourcePoint> {
	metrics::application(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		app.project_id,
		app.id,
		Range::Hour,
	)
	.await
	.ok()
	.and_then(|points| points.last().cloned())
}

/// How long the live deployment has been up: since it finished starting.
fn uptime(rows: &[DeploymentView]) -> Option<String> {
	let live = rows
		.iter()
		.find(|d| d.status == DeploymentStatus::Running)?;
	let since = live.finished_at.or(live.started_at)?;
	Some(chrome::span(OffsetDateTime::now_utc() - since))
}

/// What the health gate says about the live release: that it passes its check, or that it is
/// the previous image put back after a bad release. Nothing when the application has no gate
/// or no release is live.
fn live_health(rows: &[DeploymentView]) -> Markup {
	use podkit_core::health_gate::State;
	let Some(live) = rows.iter().find(|d| d.status == DeploymentStatus::Running) else {
		return html! {};
	};
	html! {
		@if let Some(replaced) = live.rollback_of {
			(chrome::dot(Shape::Stopped, &format!("Rolled back from #{replaced}")))
		} @else if live.gate.as_ref().is_some_and(|g| g.state == State::Healthy) {
			(chrome::dot(Shape::Running, "Healthy"))
		}
	}
}

/// The replicas of a running application set to run several copies; none otherwise.
async fn replica_status(
	s: &AppState,
	ctx: &TeamCtx,
	app: &ApplicationView,
	running: bool,
) -> Option<podkit_core::replicas::service::Live> {
	if !running {
		return None;
	}
	podkit_core::replicas::service::status(s, ctx.user.id, ctx.team.id, app.project_id, app.id)
		.await
		.ok()
		.flatten()
}

/// `#app-header`: what polling and the actions re-render. `rows` are the newest deployments
/// (newest first), for the Rollback entry and the uptime.
#[allow(
	clippy::too_many_lines,
	reason = "one markup block: the header is read top to bottom like the page it draws"
)]
pub async fn header(
	s: &AppState,
	ctx: &TeamCtx,
	app: &ApplicationView,
	rows: &[DeploymentView],
) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let link = public_link(s, &app.url);
	// An MCP server is never routed, so its header points at its MCP page instead.
	let mcp_page = podkit_core::mcp_servers::service::is_server(&s.db, app.id)
		.await
		.then(|| crate::pages::mcp::mcp_url(ctx.team.id, app.project_id, app.id));
	let busy = app.latest.is_some_and(Status::in_flight);
	let mark = application_mark(app.repo_url.as_deref(), app.build_strategy);
	let rollback_to = rows.iter().find(|d| d.can_rollback());
	let point = usage(s, ctx, app).await;
	let security =
		security::header_badge(s, ctx, app.project_id, Subject::Application(app.id)).await;
	let running = app.latest == Some(DeploymentStatus::Running);
	let dash = || "\u{2014}".to_owned();
	let figures = [
		(
			"CPU",
			point
				.as_ref()
				.and_then(|p| p.cpu)
				.filter(|_| running)
				.map_or_else(dash, |c| format!("{c:.1}%")),
		),
		(
			"Memory",
			point
				.as_ref()
				.filter(|_| running)
				.map_or_else(dash, |p| chrome::bytes(p.mem)),
		),
		(
			"Uptime",
			uptime(rows).filter(|_| running).unwrap_or_else(dash),
		),
		("Server", app.server_name.clone()),
	];
	let sha = rows
		.iter()
		.find(|d| d.status == DeploymentStatus::Running)
		.and_then(|d| d.commit_sha.as_deref())
		.map(|sha| sha.get(..7).unwrap_or(sha).to_owned());
	let newest = rows.first().filter(|d| Some(d.status) == app.latest);
	let (word, shape) = match (newest, app.latest) {
		(Some(d), _) => {
			let (shape, word) = chrome::deployment_state(d);
			(word, shape)
		}
		(None, Some(d)) => (d.label().to_owned(), chrome::deployment_shape(d)),
		(None, None) => ("Not deployed".to_owned(), Shape::Pending),
	};
	let health = live_health(rows);
	// Only an application set to run several copies has a list; the others pay nothing here.
	let replicas = replica_status(s, ctx, app, running).await;
	let kind = podkit_core::templates::service::Kind::Application;
	let newer = podkit_core::templates::service::newer(&s.db, kind, app.id.0)
		.await
		.ok()
		.flatten();
	html! {
		div id="app-header" class="flex flex-col gap-5" {
			(poll(&format!("{base}/header"), busy))
			(chrome::head(
				&logo(mark.unwrap_or(&app.name), LogoSize::Lg),
				&html! {
					(icon(if app.source_kind == SourceKind::Git { "git-branch" } else { "container" }))
					(source_label(app))
				},
				&app.name,
				&html! {
					(chrome::dot(shape, &word))
					(health)
					(security)
					@if let Some(href) = &mcp_page {
						a href=(href) class="ui-head-url" {
							span class="truncate" { "MCP server page" }
							(icon("arrow-up-right"))
						}
					} @else {
						a href=(link) class="ui-head-url" target="_blank" rel="noopener noreferrer" {
							span class="truncate" { (link) }
							(icon("external-link"))
						}
					}
					@match app.source_kind {
						SourceKind::Git => {
							span class="ui-head-ref" {
								(icon("git-branch"))
								(app.git_ref)
								@if let Some(sha) = &sha { " \u{b7} " (sha) }
							}
						}
						SourceKind::Image => {
							span class="ui-head-ref" { (app.image.as_deref().unwrap_or_default()) }
						}
					}
				},
				&actions(ctx, app, rollback_to),
			))
			@if let Some(t) = &newer {
				(crate::pages::templates::newer_banner(ctx.team.id, t))
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
			(chrome::stats("Live figures", &figures))
			@if let Some(live) = &replicas {
				(super::replica_list::chips(live))
			}
		}
	}
}

// --- dialogs --------------------------------------------------------------------------------

/// Rollback, Promote, Move and Delete, opened from the menu (and Rollback from a deployment's
/// row). They sit outside the header so a poll never closes them.
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
		(super::promote::dialog_for(ctx))
		(super::move_app::dialog_for(ctx))
		@if can_create {
			(dialog(
				"rollback-dialog",
				"Roll back to an earlier build",
				Some("Nothing is rebuilt: podkit starts the image of that deployment and replaces the running container."),
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
			sse::patch(&header(&s, &ctx, &view, &rows).await).into_response()
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
