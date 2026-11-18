//! The head of a database's page: logo, name, engine and version, status, server, and the
//! actions Start, Stop and Restart with a menu that holds Delete (a dialog that asks for the
//! name, with an optional data purge).
//!
//! Every action comes back as an SSE stream. A refusal ("a job is running", a missing
//! permission) lands in the `_action_error` signal, which the header shows, so no tab needs
//! its own line. While a job or a status change is in flight the header polls itself.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};

use super::{action_error, busy, database_url, engine_label, engine_value, load, project_url};
use crate::auth::permission::{Action, Resource};
use crate::databases::service::{self, DatabaseDetail};
use crate::databases::{Database, DatabaseStatus};
use crate::error::ServerError;
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::projects::overview::ResourceStatus;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, CHECKBOX, Dismiss, INPUT, LABEL, LogoSize, MENU_ITEM,
	MENU_ITEM_DANGER, Size, Tone, Variant, badge, button_class, dialog, dropdown_menu, icon, logo,
	menu_label, menu_separator,
};
use crate::ui::extract::TeamCtx;
use crate::ui::pages::projects::cards::status_line;
use crate::ui::sse::js_string;
use crate::ui::status::poll;
use crate::ui::{error, sse};

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{DB}{path}");
	Router::new()
		.route(&at("/header"), get(fragment))
		.route(&at("/start"), post(start))
		.route(&at("/stop"), post(stop))
		.route(&at("/restart"), post(restart))
		.route(&at("/delete"), post(remove))
}

fn fail(e: &ServerError) -> Response {
	action_error(&error::message(e)).into_response()
}

/// Start, Restart and Stop, each shown only to someone who may update the database and
/// enabled only while it can work; a stale page still gets the server's refusal in the error
/// line. Restart and Stop fold into the menu on a narrow screen.
fn actions(ctx: &TeamCtx, d: &DatabaseDetail) -> Markup {
	let database = &d.database;
	let base = database_url(ctx.team.id, database.project_id, database.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Database);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Database);
	let busy = busy(d);
	let status = database.status;
	let can_start = !busy && matches!(status, DatabaseStatus::Stopped | DatabaseStatus::Failed);
	let can_stop = !busy && status == DatabaseStatus::Running;
	let can_restart = !busy
		&& matches!(
			status,
			DatabaseStatus::Running | DatabaseStatus::Stopped | DatabaseStatus::Failed
		);
	let post = |action: &str| format!("@post({})", js_string(&format!("{base}/{action}")));
	let menu_class = button_class(Variant::Outline, Size::Icon);
	let open_delete = "$confirm = ''; $purge = false; $_delete_error = ''; el.commandForElement || document.getElementById('delete-dialog').showModal()";
	html! {
		div class="ui-app-actions" {
			@if can_update {
				button type="button" class=(if can_start { BUTTON } else { BUTTON_OUTLINE }) disabled[!can_start] "data-on:click"=(post("start"))
					"data-indicator:_acting" "data-attr:disabled"={ "$_acting || " (!can_start) } { (icon("play")) "Start" }
				button type="button" class={ (BUTTON_OUTLINE) " ui-app-secondary" } disabled[!can_restart] "data-on:click"=(post("restart"))
					"data-attr:disabled"={ "$_acting || " (!can_restart) } { (icon("refresh-cw")) "Restart" }
				button type="button" class={ (BUTTON_DANGER) " ui-app-secondary" } disabled[!can_stop] "data-on:click"=(post("stop"))
					"data-attr:disabled"={ "$_acting || " (!can_stop) } { (icon("square")) "Stop" }
			}
			@if can_update || can_delete {
				(dropdown_menu(
					"db-menu",
					&menu_class,
					&format!("Actions for {}", database.name),
					&icon("ellipsis"),
					&html! {
						(menu_label("Database"))
						@if can_update {
							button type="button" class={ (MENU_ITEM) " ui-app-collapsed" } disabled[!can_restart] "data-on:click"=(post("restart")) { (icon("refresh-cw")) "Restart" }
							button type="button" class={ (MENU_ITEM_DANGER) " ui-app-collapsed" } disabled[!can_stop] "data-on:click"=(post("stop")) { (icon("square")) "Stop" }
						}
						@if can_delete {
							(menu_separator())
							button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor="delete-dialog"
								"data-on:click"=(open_delete) { (icon("trash-2")) "Delete" }
						}
					},
				))
			}
		}
	}
}

/// `#db-header`: what polling and the actions re-render.
pub fn header(ctx: &TeamCtx, d: &DatabaseDetail) -> Markup {
	let database = &d.database;
	let base = database_url(ctx.team.id, database.project_id, database.id);
	let busy = busy(d);
	html! {
		div id="db-header" class="flex flex-col gap-3" {
			(poll(&format!("{base}/header"), busy))
			div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4" {
				div class="flex min-w-0 items-center gap-4" {
					(logo(engine_value(database.engine), LogoSize::Lg))
					div class="flex min-w-0 flex-col gap-1.5" {
						div class="flex flex-wrap items-center gap-x-3 gap-y-1" {
							h1 class="min-w-0 truncate text-2xl font-semibold tracking-tight" { (database.name) }
							(badge(Tone::Neutral, "Database"))
						}
						div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm" {
							(status_line(ResourceStatus::Database(database.status)))
							@if busy && !crate::ui::status::Status::in_flight(database.status) {
								span class="text-muted-foreground" { "A job is running" }
							}
							span class="text-muted-foreground" {
								(engine_label(database.engine)) " " (database.version) " on " (d.server_name)
							}
						}
						@if let Some(message) = &database.status_message {
							p class="text-sm break-words text-muted-foreground" { (message) }
						}
					}
				}
				(actions(ctx, d))
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
		}
	}
}

/// The delete dialog, opened from the header's menu and from the Advanced tab. It sits
/// outside the header so a poll never closes it.
pub fn dialogs(ctx: &TeamCtx, database: &Database) -> Markup {
	let can_delete = ctx.perms.can(Action::Delete, Resource::Database);
	let base = database_url(ctx.team.id, database.project_id, database.id);
	let delete = format!("@post({})", js_string(&format!("{base}/delete")));
	let differs = format!("$confirm !== {}", js_string(&database.name));
	html! {
		@if can_delete {
			(dialog(
				"delete-dialog",
				"Delete this database?",
				Some("Deleting stops and removes the database's container."),
				Dismiss::Esc,
				&html! {
					div class="flex flex-col gap-3" {
						label class=(LABEL) {
							span class="leading-normal" { "Type " strong class="font-mono break-all" { (database.name) } " to confirm" }
							input class=(INPUT) type="text" autocomplete="off" spellcheck="false" autocapitalize="off"
								aria-label="Database name" "data-bind:confirm";
						}
						label class="flex cursor-pointer items-center gap-2 text-sm" {
							input type="checkbox" class=(CHECKBOX) "data-bind:purge";
							"Also delete the data"
						}
						p class="text-sm text-destructive" "data-show"="$purge" style="display: none" {
							"The data volume is deleted too. This can't be undone."
						}
						p class="text-sm text-muted-foreground" "data-show"="!$purge" {
							"Without this, the data volume stays on the server."
						}
						p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
					}
				},
				&html! {
					form method="dialog" {
						button class=(button_class(Variant::Outline, Size::Md)) autofocus
							"data-on:click"="$confirm = ''; $purge = false; $_delete_error = ''" { "Cancel" }
					}
					button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(delete)
						"data-indicator:_deleting" "data-attr:disabled"={ "$_deleting || " (differs) } disabled { "Delete database" }
				},
			))
		}
	}
}

// --- actions ------------------------------------------------------------------------------

type DbPath = Path<(TeamId, ProjectId, DatabaseId)>;

/// What polling fetches: the header again, which drops its own poller once nothing is in
/// flight. A lost or foreign database is a real 404, so the poll stops.
async fn fragment(State(s): State<AppState>, ctx: TeamCtx, path: DbPath) -> Response {
	let Path((_, project, database)) = path;
	match load(&s, &ctx, project, database).await {
		Ok((_, d)) => sse::patch(&header(&ctx, &d)).into_response(),
		Err(response) => response,
	}
}

/// Starts a lifecycle job and reloads the page, which then follows its log.
pub(super) fn reloaded(ctx: &TeamCtx, database: &Database) -> Response {
	sse::redirect(&database_url(ctx.team.id, database.project_id, database.id)).into_response()
}

async fn start(State(s): State<AppState>, ctx: TeamCtx, path: DbPath) -> Response {
	let Path((_, project, database)) = path;
	match service::start(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => reloaded(&ctx, &d),
		Err(e) => fail(&e),
	}
}

async fn stop(State(s): State<AppState>, ctx: TeamCtx, path: DbPath) -> Response {
	let Path((_, project, database)) = path;
	match service::stop(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => reloaded(&ctx, &d),
		Err(e) => fail(&e),
	}
}

async fn restart(State(s): State<AppState>, ctx: TeamCtx, path: DbPath) -> Response {
	let Path((_, project, database)) = path;
	match service::restart(&s, ctx.user.id, ctx.team.id, project, database).await {
		Ok(d) => reloaded(&ctx, &d),
		Err(e) => fail(&e),
	}
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct DeleteSignals {
	confirm: String,
	purge: bool,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: DbPath,
	datastar::axum::ReadSignals(sig): datastar::axum::ReadSignals<DeleteSignals>,
) -> Response {
	let Path((_, project, database)) = path;
	let result = async {
		// The name is checked after a read, so a mistyped one never reaches the delete;
		// `remove` still authorizes the delete itself.
		let found = service::find(&s.db, ctx.user.id, ctx.team.id, project, database).await?;
		if sig.confirm != found.name {
			return Ok(false);
		}
		service::remove(&s, ctx.user.id, ctx.team.id, project, database, sig.purge).await?;
		Ok::<_, ServerError>(true)
	}
	.await;
	match result {
		Ok(true) => sse::redirect(&project_url(ctx.team.id, project)).into_response(),
		Ok(false) => sse::signals(&serde_json::json!({
			"_delete_error": "The name doesn't match. Type it exactly to confirm.",
		}))
		.into_response(),
		Err(e) => sse::signals(&serde_json::json!({ "_delete_error": error::message(&e) }))
			.into_response(),
	}
}
