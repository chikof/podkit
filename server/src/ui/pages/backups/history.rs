//! The Backups tab of a database: the schedule (destination, cron with its next three runs,
//! retention), the history of backups, **Back up now**, and **Restore** behind a typed
//! confirmation.
//!
//! A restore overwrites the live data, so the service refuses it unless the database's name
//! is typed; the page only asks for it in a dialog. The settings form posts to the database's
//! settings route (`pages::databases`), which stores the schedule together with the rest.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::auth::permission::{Action, Resource};
use crate::backups::BackupStatus;
use crate::backups::service::{self as backups, Entry, Overview};
use crate::databases::service as databases;
use crate::error::ServerError;
use crate::ids::{BackupId, DatabaseId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, Dismiss, INPUT, LABEL, SELECT, Size, Variant, button_class, card,
	card_content, card_footer, card_section, dialog, empty_state, icon, table,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::pages::databases::{
	Tab, database_url, forbid_unless_allowed, load, page, schedule_hint, when,
};
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{DB}{path}");
	Router::new()
		.route(&at("/backups"), get(backups_page).post(back_up_now))
		.route(&at("/backups/list"), get(backups_poll))
		.route(&at("/backups/{backup_id}/restore"), post(restore))
		.route(&at("/backups/{backup_id}/delete"), post(remove_backup))
}

/// `1.5 MB`: binary multiples, one decimal above a kilobyte.
#[allow(clippy::cast_precision_loss)]
pub(super) fn human_size(bytes: i64) -> String {
	const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
	if bytes < 1024 {
		return format!("{} B", bytes.max(0));
	}
	let mut value = bytes as f64 / 1024.0;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	format!("{value:.1} {}", UNITS[unit])
}

fn backups_url(ctx: &TeamCtx, project: ProjectId, database: DatabaseId) -> String {
	format!("{}/backups", database_url(ctx.team.id, project, database))
}

// --- the history --------------------------------------------------------------------------

fn actions(ctx: &TeamCtx, entry: &Entry) -> Markup {
	let backup = &entry.backup;
	let can_restore = ctx.perms.can(Action::Update, Resource::Database)
		&& backup.status == BackupStatus::Succeeded
		&& backup.target_id.is_some();
	let can_remove = ctx.perms.can(Action::Delete, Resource::Backup) && !backup.status.in_flight();
	let label = when(backup.created_at);
	let restore = format!(
		"$restore_id = '{}'; $confirm = ''; $_dialog_error = ''; $_restore_label = {}; el.commandForElement || document.getElementById('restore-dialog').showModal()",
		backup.id.0,
		js_string(&label)
	);
	let delete = format!(
		"$backup_id = '{}'; $_dialog_error = ''; el.commandForElement || document.getElementById('delete-backup-dialog').showModal()",
		backup.id.0
	);
	html! {
		div class="flex flex-nowrap gap-2" {
			@if can_restore {
				button type="button" class=(button_class(Variant::Outline, Size::Sm)) command="show-modal" commandfor="restore-dialog"
					aria-haspopup="dialog" "data-on:click"=(restore) { (icon("rotate-ccw")) "Restore" }
			}
			@if can_remove {
				button type="button" class=(button_class(Variant::Ghost, Size::Sm)) command="show-modal" commandfor="delete-backup-dialog"
					aria-haspopup="dialog" "data-on:click"=(delete) { (icon("trash-2")) "Delete" }
			}
		}
	}
}

fn status_cell(entry: &Entry) -> Markup {
	let backup = &entry.backup;
	html! {
		div class="flex flex-col items-start gap-1" {
			(status_badge(backup.status))
			@if let Some(message) = &backup.error {
				p class="text-xs break-words text-destructive" { (message) }
			}
			@if backup.status == BackupStatus::Succeeded && backup.target_id.is_none() {
				p class="text-xs text-muted-foreground" { "Its destination was deleted, so this backup can't be restored." }
			}
		}
	}
}

fn by(entry: &Entry) -> String {
	match (&entry.triggered_by, entry.backup.triggered_by) {
		(Some(name), _) => name.clone(),
		(None, Some(_)) => "A removed user".to_owned(),
		(None, None) => "Schedule".to_owned(),
	}
}

/// The history, re-fetched every few seconds while a backup is queued or running.
fn history(ctx: &TeamCtx, base: &str, o: &Overview) -> Markup {
	let in_flight = o.entries.iter().any(|e| e.backup.status.in_flight());
	let rows: Vec<Vec<Markup>> = o
		.entries
		.iter()
		.map(|entry| {
			let backup = &entry.backup;
			vec![
				status_cell(entry),
				html! { span class="whitespace-nowrap" { (backup.size_bytes.map_or_else(|| "—".to_owned(), human_size)) } },
				html! { span class="whitespace-nowrap" { (when(backup.created_at)) } },
				html! { span class="whitespace-nowrap" { (backup.finished_at.map_or_else(|| "—".to_owned(), when)) } },
				html! { (by(entry)) },
				actions(ctx, entry),
			]
		})
		.collect();
	html! {
		div id="backup-list" class="flex flex-col gap-3" {
			@if o.entries.is_empty() {
				(empty_state("archive", "No backups yet.", Some("A backup is a dump of this database, stored in the destination. Back one up now or set a schedule."), None))
			} @else {
				(table("backup-table", "Backups of this database", &["Status", "Size", "Started", "Finished", "By", "Actions"], &rows))
			}
			(poll(&format!("{base}/list"), in_flight))
		}
	}
}

// --- the schedule -------------------------------------------------------------------------

/// A preset for the cron field: sets it and asks for the preview.
fn preset(label: &str, cron: &str, preview: &str) -> Markup {
	let click = format!("$backup_schedule = {}; {preview}", js_string(cron));
	html! {
		button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(click) { (label) }
	}
}

async fn schedule_card(
	s: &AppState,
	ctx: &TeamCtx,
	d: &crate::databases::service::DatabaseDetail,
) -> Result<Markup, Response> {
	let database = &d.database;
	let can_update = ctx.perms.can(Action::Update, Resource::Database);
	let runs = database
		.backup_schedule
		.as_deref()
		.map(|schedule| databases::upcoming_runs(schedule, databases::PREVIEW_RUNS));
	if !can_update {
		return Ok(card(&html! {
			(card_section("Schedule", None, None))
			(card_content(&html! {
				dl class="grid grid-cols-1 gap-x-8 gap-y-3 text-sm sm:grid-cols-2" {
					div class="flex flex-col gap-0.5" {
						dt class="text-muted-foreground" { "Schedule (cron, UTC)" }
						dd class="font-mono font-medium" { (database.backup_schedule.as_deref().unwrap_or("None")) }
					}
					div class="flex flex-col gap-0.5" {
						dt class="text-muted-foreground" { "Backups kept" }
						dd class="font-medium" { (database.backup_keep) }
					}
				}
			}))
		}));
	}
	let targets = databases::backup_targets(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(ctx, &e))?;
	let db_base = database_url(ctx.team.id, database.project_id, database.id);
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{db_base}/settings"))
	);
	let preview_call = format!("@post({})", js_string(&format!("{db_base}/schedule")));
	Ok(card(&html! {
		form id="schedule" class="flex flex-col gap-5" "data-on:submit"=(save) {
			(card_section("Schedule", Some("Backups run on this cron schedule (UTC) and the oldest are removed past the retention."), None))
			(card_content(&html! {
				div class="flex flex-col gap-4" {
					label class=(LABEL) {
						"Destination"
						select class=(SELECT) "data-bind:backup_target_id" {
							button { selectedcontent {} }
							option value="" { "None" }
							@for (target, name) in &targets {
								option value=(target.0) { (name) }
							}
						}
						@if ctx.perms.can(Action::Create, Resource::Backup) {
							a class="text-sm font-normal underline" href={ "/teams/" (ctx.team.id) "/backup-targets" } { "Add an S3 destination" }
						}
					}
					label class=(LABEL) {
						"Schedule (cron, UTC)"
						input class={ (INPUT) " font-mono" } type="text" autocomplete="off" spellcheck="false" placeholder="0 3 * * *"
							"data-bind:backup_schedule" "data-on:input__debounce.400ms"=(preview_call);
						(schedule_hint(runs.as_ref()))
					}
					div class="flex flex-wrap gap-2" role="group" aria-label="Schedule presets" {
						(preset("Every hour", "0 * * * *", &preview_call))
						(preset("Daily at 03:00", "0 3 * * *", &preview_call))
						(preset("Weekly, Sunday 03:00", "0 3 * * 0", &preview_call))
					}
					label class=(LABEL) {
						"Backups to keep"
						input class=(INPUT) type="text" inputmode="numeric" placeholder="7" "data-bind:backup_keep";
					}
					(form_error())
				}
			}))
			(card_footer(&html! {
				button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save schedule" }
			}))
		}
	}))
}

// --- dialogs ------------------------------------------------------------------------------

/// A hidden element that closes the dialog around it when the server sets `_close` to the
/// dialog's id, so an action can close the form it came from.
fn closer(id: &str) -> Markup {
	let effect = format!(
		"if ($_close === {}) {{ el.closest('dialog').close(); $_close = '' }}",
		js_string(id)
	);
	html! { span hidden "data-effect"=(effect) {} }
}

fn dialog_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_dialog_error" "data-text"="$_dialog_error" style="display: none" {}
	}
}

fn restore_dialog(base: &str, name: &str) -> Markup {
	let action = format!(
		"@post({} + $restore_id + '/restore')",
		js_string(&format!("{base}/"))
	);
	// The button stays off until the name matches; the service checks it again.
	let differs = format!("$confirm !== {}", js_string(name));
	dialog(
		"restore-dialog",
		"Restore this backup?",
		None,
		Dismiss::Esc,
		&html! {
			(closer("restore-dialog"))
			div class="flex flex-col gap-3" {
				p class="font-medium text-destructive" {
					"Restoring the backup from " span "data-text"="$_restore_label" {}
					" overwrites the current data of this database."
				}
				p {
					"Everything written since that backup is lost. Type "
					strong class="break-all" { (name) }
					" to confirm."
				}
				input class=(INPUT) type="text" autocomplete="off" spellcheck="false" autocapitalize="off"
					aria-label="Database name" "data-bind:confirm";
				(dialog_error())
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus
					"data-on:click"="$restore_id = ''; $confirm = ''; $_dialog_error = ''" { "Cancel" }
			}
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(action) "data-indicator:_restoring"
				"data-attr:disabled"={ "$_restoring || " (differs) } disabled { "Restore and overwrite" }
		},
	)
}

fn delete_dialog(base: &str) -> Markup {
	let action = format!(
		"@post({} + $backup_id + '/delete')",
		js_string(&format!("{base}/"))
	);
	dialog(
		"delete-backup-dialog",
		"Delete this backup?",
		Some("Its file is removed from the destination. This can't be undone."),
		Dismiss::Esc,
		&html! {
			(closer("delete-backup-dialog"))
			(dialog_error())
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus "data-on:click"="$backup_id = ''" { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(action)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete backup" }
		},
	)
}

// --- pages and actions --------------------------------------------------------------------

async fn overview_of(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Overview, ServerError> {
	backups::overview(&s.db, ctx.user.id, ctx.team.id, project, database).await
}

async fn backups_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Backups)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let o = overview_of(&s, &ctx, project.id, d.database.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = backups_url(&ctx, project.id, d.database.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Backup);
	let can_restore = ctx.perms.can(Action::Update, Resource::Database);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Backup);
	let start = format!("@post({})", js_string(&base));
	let schedule = schedule_card(&s, &ctx, &d).await?;

	let action = html! {
		@if can_create {
			@match &o.can_start {
				Ok(()) => button type="button" class=(BUTTON) "data-on:click"=(start) "data-indicator:_starting" "data-attr:disabled"="$_starting" {
					(icon("archive")) "Back up now"
				},
				Err(why) => p class="max-w-xs text-sm text-muted-foreground" { (why) "." },
			}
		}
	};
	let content = html! {
		div class="flex flex-col gap-6" {
			(schedule)
			(card(&html! {
				(card_section("History", Some("Dumps of this database in the destination, newest first."), Some(&action)))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						(form_error())
						(history(&ctx, &base, &o))
					}
				}))
			}))
			@if can_restore { (restore_dialog(&base, &d.database.name)) }
			@if can_delete { (delete_dialog(&base)) }
		}
	};
	Ok(page(&ctx, &project, &d, Tab::Backups, &content))
}

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Markup, ServerError> {
	let o = overview_of(s, ctx, project, database).await?;
	Ok(history(ctx, &backups_url(ctx, project, database), &o))
}

async fn backups_poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match refreshed(&s, &ctx, project, database).await {
		Ok(list) => sse::patch(&list).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn back_up_now(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	let result = async {
		backups::create(&s, ctx.user.id, ctx.team.id, project, database).await?;
		refreshed(&s, &ctx, project, database).await
	}
	.await;
	match result {
		Ok(list) => sse::patch(&list)
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RestoreSignals {
	confirm: String,
}

async fn restore(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database, backup)): Path<(TeamId, ProjectId, DatabaseId, BackupId)>,
	datastar::axum::ReadSignals(sig): datastar::axum::ReadSignals<RestoreSignals>,
) -> Response {
	let result = backups::restore_confirmed(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		database,
		backup,
		&sig.confirm,
	)
	.await;
	match result {
		Ok(_) => sse::signals(&json!({
			"_dialog_error": "", "restore_id": "", "confirm": "", "_close": "restore-dialog",
		}))
		.and(sse::toast(
			"Restore queued. The database is overwritten when it runs.",
		))
		.into_response(),
		Err(e) => sse::signals(&json!({ "_dialog_error": error::message(&e) })).into_response(),
	}
}

async fn remove_backup(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database, backup)): Path<(TeamId, ProjectId, DatabaseId, BackupId)>,
) -> Response {
	let result = async {
		backups::remove(&s, ctx.user.id, ctx.team.id, project, database, backup).await?;
		refreshed(&s, &ctx, project, database).await
	}
	.await;
	match result {
		Ok(list) => sse::patch(&list)
			.and(sse::signals(&json!({
				"_dialog_error": "", "backup_id": "", "_close": "delete-backup-dialog",
			})))
			.and(sse::toast("Backup deleted"))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_dialog_error": error::message(&e) })).into_response(),
	}
}
