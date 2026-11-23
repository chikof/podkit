//! The Backups section of a volume, on the application's Volumes tab and the stack's
//! Volumes tab: where the volume is backed up to, when, how many are kept, whether its
//! owner is stopped for the copy, the history with **Restore** and **Delete**, and
//! **Back up now**.
//!
//! One card per volume, all patched in place by their own id (`vb-<volume>`) and
//! re-fetched every few seconds while a backup or restore runs. The three dialogs
//! (settings, restore, delete) exist once per page and are pointed at a volume by the
//! `vb_url` signal, which the volume's own buttons set before they open one.
//!
//! A restore replaces files, so the service refuses it unless the volume's mount path
//! or compose name is typed; the dialog only asks for it. S3 secrets never get here:
//! the page names a destination and nothing else about it.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::permission::{Action, Resource};
use crate::backups::BackupStatus;
use crate::ids::{BackupTargetId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, INPUT, LABEL, MENU_ITEM, MENU_ITEM_DANGER, SELECT, Size,
	Status as Shape, Variant, button_class, card, card_content, card_section, dialog,
	dropdown_menu, empty_state, icon, table,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::apps::chrome;
use crate::ui::pages::databases::{schedule_hint, when};
use crate::ui::sse::{self, js_string};
use crate::ui::status::{Status, poll as poll_when};
use crate::volume_backups::service::{self, Entry, RestoreInput, SettingsInput, View};
use crate::volume_backups::subject::Vol;
use crate::volume_backups::{Consistency, Restore, RestoreMode, VolRoute, subject::Owner};

const APP: &str =
	"/teams/{team_id}/projects/{project_id}/apps/{app_id}/volumes/{volume_id}/backups";
const STACK: &str =
	"/teams/{team_id}/projects/{project_id}/stacks/{stack_id}/volumes/{volume_key}/backups";

/// What the volumes belong to, for the words on the page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	App,
	Stack,
}

impl Kind {
	const fn owner(self) -> &'static str {
		match self {
			Self::App => "The application",
			Self::Stack => "The stack",
		}
	}

	const fn new_volume_label(self) -> &'static str {
		match self {
			Self::App => "Mount path of the new volume",
			Self::Stack => "Name of the new volume",
		}
	}

	const fn new_volume_hint(self) -> &'static str {
		match self {
			Self::App => "/restored",
			Self::Stack => "restored-data",
		}
	}
}

pub fn router() -> Router<AppState> {
	let mut router = Router::new();
	for base in [APP, STACK] {
		router = router
			.route(base, post(back_up))
			.route(&format!("{base}/list"), get(poll))
			.route(&format!("{base}/settings"), post(save_settings))
			.route(&format!("{base}/schedule"), post(schedule_preview))
			.route(&format!("{base}/{{backup_id}}/restore"), post(restore))
			.route(&format!("{base}/{{backup_id}}/delete"), post(remove));
	}
	router
}

/// The signals the section and its dialogs read; a page puts them on the element
/// that holds the section.
#[must_use]
pub fn signals() -> Value {
	json!({
		"vb_url": "", "vb_target": "", "vb_schedule": "", "vb_keep": "", "vb_consistency": "live",
		"vb_backup": "", "vb_label": "", "vb_when": "", "vb_confirm": "", "vb_mode": "same",
		"vb_new": "", "_vb_error": "", "_vb_close": "", "_vb_note": "",
	})
}

/// `1.5 MB`: binary multiples, one decimal above a kilobyte.
#[allow(clippy::cast_precision_loss)]
fn human_size(bytes: i64) -> String {
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

/// The id of a volume in a url and in the section's element id: an application's volume
/// id, or a stack volume's compose name.
fn ident(v: &View) -> String {
	v.subject
		.volume_id
		.map_or_else(|| v.subject.label.clone(), |id| id.0.to_string())
}

/// `…/volumes/<volume>/backups` of the volume in `volumes_base` (`…/volumes`).
fn volume_url(volumes_base: &str, v: &View) -> String {
	format!("{volumes_base}/{}/backups", ident(v))
}

/// The same url for a request's route, for the handlers that answer with a section.
fn route_url(ctx: &TeamCtx, route: &VolRoute) -> String {
	let team = ctx.team.id;
	match &route.vol {
		Vol::App { app, volume } => format!(
			"/teams/{team}/projects/{}/apps/{app}/volumes/{volume}/backups",
			route.project
		),
		Vol::Stack { stack, key } => format!(
			"/teams/{team}/projects/{}/stacks/{stack}/volumes/{key}/backups",
			route.project
		),
	}
}

const fn shape(status: BackupStatus) -> Shape {
	match status {
		BackupStatus::Queued => Shape::Pending,
		BackupStatus::Running => Shape::Building,
		BackupStatus::Succeeded => Shape::Running,
		BackupStatus::Failed => Shape::Failed,
	}
}

const fn consistency_word(c: Consistency) -> &'static str {
	match c {
		Consistency::Live => "Live: it keeps running",
		Consistency::Stop => "Stopped for the copy",
	}
}

fn trigger(entry: &Entry) -> String {
	match (&entry.triggered_by, entry.backup.triggered_by) {
		(Some(name), _) => format!("manual \u{b7} {name}"),
		(None, Some(_)) => "manual \u{b7} a removed user".to_owned(),
		(None, None) => "scheduled".to_owned(),
	}
}

// --- the section --------------------------------------------------------------------------

fn history_actions(ctx: &TeamCtx, url: &str, v: &View, entry: &Entry) -> Markup {
	let backup = &entry.backup;
	let can_restore = ctx.perms.can(Action::Update, Resource::Volume)
		&& backup.status == BackupStatus::Succeeded
		&& backup.target_id.is_some();
	let can_remove = ctx.perms.can(Action::Delete, Resource::Backup) && !backup.status.in_flight();
	let label = when(backup.created_at);
	let open = |dialog: &str, extra: &str| {
		format!(
			"$vb_url = {}; $vb_backup = '{}'; $vb_label = {}; $vb_when = {}; $vb_confirm = ''; $vb_mode = 'same'; $vb_new = ''; $_vb_error = ''; {extra}el.commandForElement || document.getElementById('{dialog}').showModal()",
			js_string(url),
			backup.id.0,
			js_string(&v.subject.label),
			js_string(&label),
		)
	};
	let restore = open("vb-restore-dialog", "");
	let delete = open("vb-delete-dialog", "");
	html! {
		@if can_restore || can_remove {
			(dropdown_menu(
				&format!("vb-menu-{}", backup.id.0),
				&button_class(Variant::Outline, Size::Icon),
				&format!("Actions for the backup of {label}"),
				&icon("ellipsis"),
				&html! {
					@if can_restore {
						button type="button" class=(MENU_ITEM) command="show-modal" commandfor="vb-restore-dialog"
							aria-haspopup="dialog" "data-on:click"=(restore) { (icon("rotate-ccw")) "Restore" }
					}
					@if can_remove {
						button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor="vb-delete-dialog"
							aria-haspopup="dialog" "data-on:click"=(delete) { (icon("trash-2")) "Delete" }
					}
				},
			))
		}
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
			@if let Some(warning) = &backup.warning {
				p class="ui-hint" { (warning) }
			}
			@if backup.status == BackupStatus::Succeeded && backup.target_id.is_none() {
				p class="ui-hint" { "Its destination was deleted, so this backup can't be restored." }
			}
		}
	}
}

fn size_cell(entry: &Entry) -> Markup {
	let backup = &entry.backup;
	html! {
		div class="ui-stat-cell" {
			span class="whitespace-nowrap" { (backup.size_bytes.map_or_else(|| "\u{2014}".to_owned(), human_size)) }
			@if let (Some(entries), Some(sha)) = (backup.entries, &backup.sha256) {
				span class="ui-hint" title=(sha) { (entries) " items \u{b7} sha256 " code { (sha.get(..12).unwrap_or(sha)) } }
			}
		}
	}
}

fn restore_line(r: &Restore) -> Markup {
	let how = match r.mode {
		RestoreMode::Same => "into the same volume",
		RestoreMode::New => "into a new volume",
	};
	html! {
		li {
			(r.status.label()) " " (how) " \u{b7} " (when(r.created_at))
			@if let Some(error) = &r.error {
				" \u{b7} " span class="text-destructive" { (error) }
			}
		}
	}
}

/// The card of one volume.
#[must_use]
pub fn section(ctx: &TeamCtx, url: &str, v: &View) -> Markup {
	let can_start = ctx.perms.can(Action::Create, Resource::Backup);
	let can_configure = ctx.perms.can(Action::Update, Resource::Backup);
	let in_flight = v.entries.iter().any(|e| e.backup.status.in_flight())
		|| v.restores.iter().any(|r| !r.status.is_final());
	let policy = v.policy.as_ref();
	let consistency = policy.map_or(Consistency::Live, |p| p.consistency);
	let keep = policy.map_or(crate::volume_backups::DEFAULT_KEEP, |p| p.keep);
	let schedule = policy.and_then(|p| p.schedule.as_deref());

	let start = format!("@post({})", js_string(url));
	let settings = format!(
		"$vb_url = {}; $vb_target = '{}'; $vb_schedule = {}; $vb_keep = '{keep}'; $vb_consistency = '{}'; $_vb_error = ''; @post({}); el.commandForElement || document.getElementById('vb-settings-dialog').showModal()",
		js_string(url),
		policy
			.and_then(|p| p.target_id)
			.map_or(String::new(), |t| t.0.to_string()),
		js_string(schedule.unwrap_or("")),
		consistency.as_str(),
		js_string(&format!("{url}/schedule")),
	);
	let action = html! {
		div class="flex flex-wrap items-center gap-2" {
			@if can_start {
				@match &v.can_start {
					Ok(()) => button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(start)
						"data-indicator:_vb_starting" "data-attr:disabled"="$_vb_starting" { (icon("archive")) "Back up now" },
					Err(why) => p class="ui-hint ui-card-why" { (why) "." },
				}
			}
			@if can_configure {
				button type="button" class=(button_class(Variant::Outline, Size::Sm)) command="show-modal" commandfor="vb-settings-dialog"
					aria-haspopup="dialog" "data-on:click"=(settings) { (icon("settings")) "Settings" }
			}
		}
	};

	let rows: Vec<Vec<Markup>> = v
		.entries
		.iter()
		.map(|entry| {
			vec![
				html! { span class="whitespace-nowrap" { (when(entry.backup.created_at)) } },
				html! { (trigger(entry)) },
				size_cell(entry),
				status_cell(entry),
				history_actions(ctx, url, v, entry),
			]
		})
		.collect();
	let id = ident(v);
	html! {
		div id={ "vb-" (id) } {
			(card(&html! {
				(card_section(&format!("Backups \u{b7} {}", v.subject.label), None, Some(&action)))
				(card_content(&html! {
					dl class="ui-facts ui-vb-facts" {
						div {
							dt { "Destination" }
							dd { (v.target_name.as_deref().unwrap_or("None")) }
						}
						div {
							dt { "Schedule (cron, UTC)" }
							dd {
								@if let Some(schedule) = schedule { code { (schedule) } } @else { "None" }
								@if let Some(next) = v.next_runs.first() { " \u{b7} next " (when(*next)) }
							}
						}
						div {
							dt { "Kept" }
							dd { (keep) }
						}
						div {
							dt { "While backing up" }
							dd { (consistency_word(consistency)) }
						}
					}
				}))
				@if v.entries.is_empty() {
					(card_content(&empty_state("archive", "No backups yet.", Some("A backup is a compressed copy of this volume's files, stored in the destination. Back one up now or set a schedule."), None)))
				} @else {
					(card_content(&table(&format!("vb-table-{id}"), &format!("Backups of {}", v.subject.label), &["Started", "Trigger", "Size", "Status", ""], &rows)))
				}
				@if !v.restores.is_empty() {
					(card_content(&html! {
						p class="ui-hint" { "Restores" }
						ul class="ui-hint" { @for r in &v.restores { (restore_line(r)) } }
					}))
				}
				(poll_when(&format!("{url}/list"), in_flight))
			}))
		}
	}
}

// --- the dialogs --------------------------------------------------------------------------

/// A hidden element that closes the dialog around it when the server sets `_vb_close` to
/// the dialog's id.
fn closer(id: &str) -> Markup {
	let effect = format!(
		"if ($_vb_close === {}) {{ el.closest('dialog').close(); $_vb_close = '' }}",
		js_string(id)
	);
	html! { span hidden "data-effect"=(effect) {} }
}

fn dialog_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_vb_error" "data-text"="$_vb_error" style="display: none" {}
	}
}

fn preset(label: &str, cron: &str, preview: &str) -> Markup {
	let click = format!("$vb_schedule = {}; {preview}", js_string(cron));
	html! { button type="button" class="ui-chip" "data-on:click"=(click) { (label) } }
}

fn settings_dialog(
	team: TeamId,
	destinations: &[(BackupTargetId, String)],
	can_add: bool,
) -> Markup {
	let save = "evt.preventDefault(); @post($vb_url + '/settings')";
	let preview = "@post($vb_url + '/schedule')";
	dialog(
		"vb-settings-dialog",
		"Volume backups",
		Some("Where this volume is backed up, when, and how many copies are kept."),
		&html! {
			(closer("vb-settings-dialog"))
			form id="vb-settings-form" class="flex flex-col gap-4" "data-on:submit"=(save) {
				label class=(LABEL) {
					"Destination"
					select class=(SELECT) autofocus "data-bind:vb_target" {
						button { selectedcontent {} }
						option value="" { "None" }
						@for (target, name) in destinations {
							option value=(target.0) { (name) }
						}
					}
					@if can_add {
						a class="ui-link" href={ "/teams/" (team) "/backup-targets" } { "Add an S3 destination" }
					}
				}
				label class=(LABEL) {
					"Schedule (cron, UTC)"
					input class={ (INPUT) " font-mono" } type="text" autocomplete="off" spellcheck="false" placeholder="0 3 * * *"
						"data-bind:vb_schedule" "data-on:input__debounce.400ms"=(preview);
					(schedule_hint(None))
				}
				div class="ui-chips" role="group" aria-label="Schedule presets" {
					(preset("Every hour", "0 * * * *", preview))
					(preset("Daily at 03:00", "0 3 * * *", preview))
					(preset("Weekly, Sunday 03:00", "0 3 * * 0", preview))
				}
				label class=(LABEL) {
					"Backups to keep"
					input class=(INPUT) type="text" inputmode="numeric" placeholder="7" autocomplete="off" "data-bind:vb_keep";
				}
				label class=(LABEL) {
					"While backing up"
					select class=(SELECT) "data-bind:vb_consistency" {
						button { selectedcontent {} }
						option value="live" { "Live: keep it running (default)" }
						option value="stop" { "Stop it during the backup" }
					}
					span class="text-xs font-normal text-muted-foreground" {
						"Live reads the files as they are, so something that writes while it is read can end up inconsistent in the copy; the backup says when that may have happened. Stopping takes a short downtime for a consistent copy."
					}
				}
				(dialog_error())
			}
		},
		&html! {
			form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) { "Cancel" } }
			button type="submit" form="vb-settings-form" class=(BUTTON) "data-indicator:_vb_saving" "data-attr:disabled"="$_vb_saving" { (icon("save")) "Save" }
		},
	)
}

fn restore_dialog(kind: Kind) -> Markup {
	let action = "@post($vb_url + '/' + $vb_backup + '/restore')";
	// The button stays off until the path or name matches; the service checks it again.
	let off = "$_vb_restoring || $vb_confirm !== $vb_label || ($vb_mode === 'new' && $vb_new.trim() === '')";
	dialog(
		"vb-restore-dialog",
		"Restore this backup?",
		None,
		&html! {
			(closer("vb-restore-dialog"))
			div class="flex flex-col gap-3" {
				p {
					"Restoring the backup from " strong "data-text"="$vb_when" {} " writes its files back."
				}
				label class=(LABEL) {
					"Restore"
					select class=(SELECT) "data-bind:vb_mode" {
						button { selectedcontent {} }
						option value="same" { "Into this volume, replacing its files" }
						option value="new" { "Into a new volume, leaving this one alone" }
					}
				}
				p class="font-medium text-destructive" "data-show"="$vb_mode === 'same'" {
					"Everything in this volume now is deleted first. " (kind.owner()) " is stopped while the files are written and started again after."
				}
				label class=(LABEL) "data-show"="$vb_mode === 'new'" style="display: none" {
					(kind.new_volume_label())
					input class={ (INPUT) " font-mono" } type="text" autocomplete="off" spellcheck="false" autocapitalize="off"
						placeholder=(kind.new_volume_hint()) "data-bind:vb_new";
				}
				p {
					"Type " strong class="break-all" "data-text"="$vb_label" {} " to confirm."
				}
				input class=(INPUT) type="text" autocomplete="off" spellcheck="false" autocapitalize="off"
					aria-label="Volume name" "data-bind:vb_confirm";
				(dialog_error())
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Cancel" }
			}
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(action) "data-indicator:_vb_restoring"
				"data-attr:disabled"=(off) disabled { "Restore" }
		},
	)
}

fn delete_dialog() -> Markup {
	let action = "@post($vb_url + '/' + $vb_backup + '/delete')";
	dialog(
		"vb-delete-dialog",
		"Delete this backup?",
		Some("Its file is removed from the destination. This can't be undone."),
		&html! {
			(closer("vb-delete-dialog"))
			p { "The backup from " strong "data-text"="$vb_when" {} "." }
			(dialog_error())
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(action)
				"data-indicator:_vb_deleting" "data-attr:disabled"="$_vb_deleting" { "Delete backup" }
		},
	)
}

/// Every volume's card and the three dialogs, for a Volumes tab. `volumes_base` is the
/// tab's own url (`…/apps/<id>/volumes`); `destinations` the names of the team's S3
/// destinations. A volume list that is empty renders nothing at all.
#[must_use]
pub fn panel(
	ctx: &TeamCtx,
	kind: Kind,
	volumes_base: &str,
	views: &[View],
	destinations: &[(BackupTargetId, String)],
) -> Markup {
	if views.is_empty() {
		return html! {};
	}
	let can_restore = ctx.perms.can(Action::Update, Resource::Volume);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Backup);
	let can_configure = ctx.perms.can(Action::Update, Resource::Backup);
	html! {
		div class="flex flex-col gap-6" data-signals=(signals().to_string()) {
			p class="text-sm text-destructive" role="alert" "data-show"="$_vb_note" "data-text"="$_vb_note" style="display: none" {}
			@for v in views { (section(ctx, &volume_url(volumes_base, v), v)) }
			@if can_configure { (settings_dialog(ctx.team.id, destinations, ctx.perms.can(Action::Create, Resource::Backup))) }
			@if can_restore { (restore_dialog(kind)) }
			@if can_delete { (delete_dialog()) }
		}
	}
}

// --- actions ------------------------------------------------------------------------------

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	route: &VolRoute,
) -> Result<Markup, crate::error::ServerError> {
	let view = service::overview(s, ctx.user.id, ctx.team.id, route.project, &route.vol).await?;
	Ok(section(ctx, &route_url(ctx, route), &view))
}

/// A failure of an action on the page, said above the cards.
fn note(e: &crate::error::ServerError) -> Response {
	sse::signals(&json!({ "_vb_note": error::message(e) })).into_response()
}

async fn back_up(State(s): State<AppState>, ctx: TeamCtx, route: VolRoute) -> Response {
	let result = async {
		service::create(&s, ctx.user.id, ctx.team.id, route.project, &route.vol).await?;
		refreshed(&s, &ctx, &route).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card)
			.and(sse::signals(&json!({ "_vb_note": "" })))
			.into_response(),
		Err(e) => note(&e),
	}
}

async fn poll(State(s): State<AppState>, ctx: TeamCtx, route: VolRoute) -> Response {
	match refreshed(&s, &ctx, &route).await {
		Ok(card) => sse::patch(&card).into_response(),
		Err(e) => note(&e),
	}
}

fn number(value: &Value) -> Option<i64> {
	match value {
		Value::Number(n) => n.as_i64(),
		Value::String(s) => s.trim().parse().ok(),
		_ => None,
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the fields are the page's `vb_` signals"
)]
struct SettingsSignals {
	vb_target: Value,
	vb_schedule: String,
	vb_keep: Value,
	vb_consistency: String,
}

impl SettingsSignals {
	fn input(&self) -> Result<SettingsInput, crate::error::ServerError> {
		let keep = match &self.vb_keep {
			Value::String(s) if s.trim().is_empty() => None,
			Value::Null => None,
			other => Some(
				number(other)
					.and_then(|n| i32::try_from(n).ok())
					.ok_or_else(|| {
						crate::error::ServerError::Validation(
							"keep must be a whole number of backups".to_owned(),
						)
					})?,
			),
		};
		Ok(SettingsInput {
			target_id: number(&self.vb_target).map(BackupTargetId),
			schedule: Some(self.vb_schedule.trim().to_owned()).filter(|s| !s.is_empty()),
			keep,
			consistency: Consistency::parse(&self.vb_consistency),
		})
	}
}

async fn save_settings(
	State(s): State<AppState>,
	ctx: TeamCtx,
	route: VolRoute,
	ReadSignals(sig): ReadSignals<SettingsSignals>,
) -> Response {
	let result = async {
		let input = sig.input()?;
		service::save_settings(
			&s,
			ctx.user.id,
			ctx.team.id,
			route.project,
			&route.vol,
			input,
		)
		.await?;
		refreshed(&s, &ctx, &route).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card)
			.and(sse::signals(
				&json!({ "_vb_error": "", "_vb_close": "vb-settings-dialog" }),
			))
			.and(sse::toast("Backup settings saved"))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_vb_error": error::message(&e) })).into_response(),
	}
}

async fn schedule_preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	route: VolRoute,
	ReadSignals(sig): ReadSignals<SettingsSignals>,
) -> Response {
	let schedule = sig.vb_schedule.trim();
	if schedule.is_empty() {
		return sse::patch(&schedule_hint(None)).into_response();
	}
	match service::preview_schedule(
		&s,
		ctx.user.id,
		ctx.team.id,
		route.project,
		&route.vol,
		schedule,
	)
	.await
	{
		Ok(runs) => sse::patch(&schedule_hint(Some(&runs))).into_response(),
		Err(e) => note(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the fields are the page's `vb_` signals"
)]
struct RestoreSignals {
	vb_confirm: String,
	vb_mode: String,
	vb_new: String,
}

async fn restore(
	State(s): State<AppState>,
	ctx: TeamCtx,
	route: VolRoute,
	ReadSignals(sig): ReadSignals<RestoreSignals>,
) -> Response {
	let mode = if sig.vb_mode == "new" {
		RestoreMode::New
	} else {
		RestoreMode::Same
	};
	let result = async {
		let backup = route.backup()?;
		service::restore(
			&s,
			ctx.user.id,
			ctx.team.id,
			route.project,
			&route.vol,
			backup,
			RestoreInput {
				mode,
				new_volume: Some(sig.vb_new.clone()).filter(|n| !n.trim().is_empty()),
				confirm: sig.vb_confirm.clone(),
			},
		)
		.await?;
		refreshed(&s, &ctx, &route).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card)
			.and(sse::signals(&json!({
				"_vb_error": "", "vb_confirm": "", "vb_new": "", "_vb_close": "vb-restore-dialog",
			})))
			.and(sse::toast("Restore queued"))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_vb_error": error::message(&e) })).into_response(),
	}
}

async fn remove(State(s): State<AppState>, ctx: TeamCtx, route: VolRoute) -> Response {
	let result = async {
		let backup = route.backup()?;
		service::remove(
			&s,
			ctx.user.id,
			ctx.team.id,
			route.project,
			&route.vol,
			backup,
		)
		.await?;
		refreshed(&s, &ctx, &route).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card)
			.and(sse::signals(
				&json!({ "_vb_error": "", "_vb_close": "vb-delete-dialog" }),
			))
			.and(sse::toast("Backup deleted"))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_vb_error": error::message(&e) })).into_response(),
	}
}

/// What the Volumes tab of an application or stack needs to draw its backup cards.
///
/// # Errors
/// The service's: not allowed, or not found.
pub async fn views_for(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	owner: Owner,
) -> Result<(Vec<View>, Vec<(BackupTargetId, String)>), crate::error::ServerError> {
	let views = match owner {
		Owner::App(app) => {
			service::overviews_of_application(s, ctx.user.id, ctx.team.id, project, app).await?
		}
		Owner::Stack(stack) => {
			service::overviews_of_stack(s, ctx.user.id, ctx.team.id, project, stack).await?
		}
	};
	let destinations = if ctx.perms.can(Action::Update, Resource::Backup) {
		service::destinations(s, ctx.user.id, ctx.team.id).await?
	} else {
		Vec::new()
	};
	Ok((views, destinations))
}
