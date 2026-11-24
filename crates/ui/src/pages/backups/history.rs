//! The Backups tab of a database: the history of backups with **Back up now** and **Restore**
//! behind a typed confirmation, beside the schedule (destination, cron with its next three
//! runs, retention).
//!
//! A restore overwrites the live data, so the service refuses it unless the database's name
//! is typed; the page only asks for it in a dialog. Saving the schedule posts the settings
//! to this tab's own route, which stores them like the General tab does but answers by
//! patching the history section (`#backup-section`): picking a destination is what lets a
//! backup start, so the section's button and its message change without a reload.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::components::{
	BUTTON, BUTTON_DANGER, INPUT, LABEL, MENU_ITEM, MENU_ITEM_DANGER, SELECT, Size,
	Status as Shape, Variant, button_class, card, card_content, card_section, dialog,
	dropdown_menu, empty_state, icon, table,
};
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::pages::apps::chrome;
use crate::pages::databases::{
	SettingsSignals, Tab, database_url, forbid_unless_allowed, load, page, reloaded, schedule_hint,
	when,
};
use crate::sse::js_string;
use crate::status::{Status, poll};
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::backups::BackupStatus;
use podkit_core::backups::service::{self as backups, Entry, Overview};
use podkit_core::databases::service as databases;
use podkit_core::error::ServerError;
use podkit_core::ids::{BackupId, DatabaseId, ProjectId, TeamId};
use podkit_core::state::AppState;

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{DB}{path}");
	Router::new()
		.route(&at("/backups"), get(backups_page).post(back_up_now))
		.route(&at("/backups/list"), get(backups_poll))
		.route(&at("/backups/settings"), post(save_settings))
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

/// The row menu: Restore (typed confirmation) and Delete, each only for someone who may use
/// it and only while it can work.
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
		@if can_restore || can_remove {
			(dropdown_menu(
				&format!("backup-menu-{}", backup.id.0),
				&button_class(Variant::Outline, Size::Icon),
				&format!("Actions for the backup of {label}"),
				&icon("ellipsis"),
				&html! {
					@if can_restore {
						button type="button" class=(MENU_ITEM) command="show-modal" commandfor="restore-dialog"
							aria-haspopup="dialog" "data-on:click"=(restore) { (icon("rotate-ccw")) "Restore" }
					}
					@if can_remove {
						button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor="delete-backup-dialog"
							aria-haspopup="dialog" "data-on:click"=(delete) { (icon("trash-2")) "Delete" }
					}
				},
			))
		}
	}
}

/// A backup's state as Quiet's shaped dot and the word.
const fn shape(status: BackupStatus) -> Shape {
	match status {
		BackupStatus::Queued => Shape::Pending,
		BackupStatus::Running => Shape::Building,
		BackupStatus::Succeeded => Shape::Running,
		BackupStatus::Failed => Shape::Failed,
	}
}

fn status_cell(entry: &Entry) -> Markup {
	let backup = &entry.backup;
	html! {
		div class="ui-stat-cell" {
			(chrome::dot(shape(backup.status), Status::label(backup.status)))
			@if let Some(message) = &backup.error {
				p class="ui-cell-err" { (message) }
			}
			@if backup.status == BackupStatus::Succeeded && backup.target_id.is_none() {
				p class="ui-hint" { "Its destination was deleted, so this backup can't be restored." }
			}
		}
	}
}

/// What started the backup: the schedule, or a person.
fn trigger(entry: &Entry) -> String {
	match (&entry.triggered_by, entry.backup.triggered_by) {
		(Some(name), _) => format!("manual \u{b7} {name}"),
		(None, Some(_)) => "manual \u{b7} a removed user".to_owned(),
		(None, None) => "scheduled".to_owned(),
	}
}

/// The history card: the list, and the button that starts a backup or says why it cannot.
/// It is re-fetched every few seconds while a backup is queued or running, and patched whole
/// when the destination changes.
fn section(ctx: &TeamCtx, base: &str, o: &Overview) -> Markup {
	let in_flight = o.entries.iter().any(|e| e.backup.status.in_flight());
	let can_create = ctx.perms.can(Action::Create, Resource::Backup);
	let start = format!("@post({})", js_string(base));
	let rows: Vec<Vec<Markup>> = o
		.entries
		.iter()
		.map(|entry| {
			let backup = &entry.backup;
			vec![
				html! { span class="whitespace-nowrap" { (when(backup.created_at)) } },
				html! { (trigger(entry)) },
				html! { span class="whitespace-nowrap" { (backup.size_bytes.map_or_else(|| "\u{2014}".to_owned(), human_size)) } },
				status_cell(entry),
				actions(ctx, entry),
			]
		})
		.collect();
	let action = html! {
		@if can_create {
			@match &o.can_start {
				Ok(()) => button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(start) "data-indicator:_starting" "data-attr:disabled"="$_starting" {
					(icon("archive")) "Back up now"
				},
				Err(why) => p class="ui-hint ui-card-why" { (why) "." },
			}
		}
	};
	html! {
		div id="backup-section" {
			(card(&html! {
				(card_section("Backups", None, Some(&action)))
				(form_error())
				@if o.entries.is_empty() {
					(card_content(&empty_state("archive", "No backups yet.", Some("A backup is a dump of this database, stored in the destination. Back one up now or set a schedule."), None)))
				} @else {
					(card_content(&table("backup-table", "Backups of this database", &["Started", "Trigger", "Size", "Status", ""], &rows)))
				}
				(poll(&format!("{base}/list"), in_flight))
			}))
		}
	}
}

// --- the schedule -------------------------------------------------------------------------

/// A preset for the cron field: sets it and asks for the preview.
fn preset(label: &str, cron: &str, preview: &str) -> Markup {
	let click = format!("$backup_schedule = {}; {preview}", js_string(cron));
	html! {
		button type="button" class="ui-chip" "data-on:click"=(click) { (label) }
	}
}

async fn schedule_card(
	s: &AppState,
	ctx: &TeamCtx,
	d: &podkit_core::databases::service::DatabaseDetail,
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
				dl class="ui-facts ui-facts-col" {
					div {
						dt { "Schedule (cron, UTC)" }
						dd { code { (database.backup_schedule.as_deref().unwrap_or("None")) } }
					}
					div {
						dt { "Backups kept" }
						dd { (database.backup_keep) }
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
		js_string(&format!("{db_base}/backups/settings"))
	);
	let preview_call = format!("@post({})", js_string(&format!("{db_base}/schedule")));
	Ok(card(&html! {
		form id="schedule" "data-on:submit"=(save) {
			(card_section("Schedule", Some("Backups run on this cron schedule (UTC) and the oldest are removed past the retention."), None))
			(card_content(&html! {
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
						a class="ui-link" href={ "/teams/" (ctx.team.id) "/backup-targets" } { "Add an S3 destination" }
					}
				}
				label class=(LABEL) {
					"Schedule (cron, UTC)"
					input class={ (INPUT) " font-mono" } type="text" autocomplete="off" spellcheck="false" placeholder="0 3 * * *"
						"data-bind:backup_schedule" "data-on:input__debounce.400ms"=(preview_call);
					(schedule_hint(runs.as_ref()))
				}
				div class="ui-chips" role="group" aria-label="Schedule presets" {
					(preset("Every hour", "0 * * * *", &preview_call))
					(preset("Daily at 03:00", "0 3 * * *", &preview_call))
					(preset("Weekly, Sunday 03:00", "0 3 * * 0", &preview_call))
				}
				label class=(LABEL) {
					"Backups to keep"
					input class=(INPUT) type="text" inputmode="numeric" placeholder="7" "data-bind:backup_keep";
				}
				(form_error())
				button type="submit" class={ (BUTTON) " ui-block" } "data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("save")) "Save schedule" }
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
	let can_restore = ctx.perms.can(Action::Update, Resource::Database);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Backup);
	let schedule = schedule_card(&s, &ctx, &d).await?;
	let drills = super::drills::overview_of(&s, &ctx, project.id, d.database.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;

	let content = html! {
		div class="ui-two ui-two-wide" {
			(section(&ctx, &base, &o))
			(schedule)
			@if can_restore { (restore_dialog(&base, &d.database.name)) }
			@if can_delete { (delete_dialog(&base)) }
		}
		(super::drills::block(&ctx, project.id, d.database.id, &drills))
	};
	Ok(page(&s, &ctx, &project, &d, Tab::Backups, &content).await)
}

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Markup, ServerError> {
	let o = overview_of(s, ctx, project, database).await?;
	Ok(section(ctx, &backups_url(ctx, project, database), &o))
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

/// Saves the schedule form. The settings are stored exactly as the General tab stores them
/// (one service call, so a port or limit changed there is not lost), and the answer patches
/// the next-runs hint and the history section: with a destination now set, the section
/// offers **Back up now** without the page being reloaded.
async fn save_settings(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	ReadSignals(sig): ReadSignals<SettingsSignals>,
) -> Response {
	let result = async {
		let input = sig.input()?;
		// A bad cron is the field's problem; say it under the field.
		let runs = sig
			.schedule()
			.map(|schedule| databases::upcoming_runs(schedule, databases::PREVIEW_RUNS));
		if let Some(Err(_)) = &runs {
			return Ok(Err(schedule_hint(runs.as_ref())));
		}
		let saved =
			databases::update_settings(&s, ctx.user.id, ctx.team.id, project, database, input)
				.await?;
		Ok::<_, ServerError>(Ok((saved, runs)))
	}
	.await;
	match result {
		Ok(Err(hint)) => sse::patch(&hint)
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response(),
		Ok(Ok(((updated, true), _))) => reloaded(&ctx, &updated),
		Ok(Ok(((_, false), runs))) => match refreshed(&s, &ctx, project, database).await {
			Ok(history) => sse::patch(&schedule_hint(runs.as_ref()))
				.and(sse::patch(&history))
				.and(sse::signals(&json!({ "_error": "" })))
				.and(sse::toast("Schedule saved"))
				.into_response(),
			Err(e) => error::action(&e),
		},
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
	ReadSignals(sig): ReadSignals<RestoreSignals>,
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
