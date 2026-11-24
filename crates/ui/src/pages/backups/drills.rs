//! The restore drills card of a database's Backups tab: the last drill's result, the
//! history, **Run drill now** and the drill schedule.
//!
//! A drill restores the newest backup into a throwaway copy and counts its tables (see
//! `backups::drills`), so the card says plainly that the database itself is not touched.
//! The result and history (`#drill-section`) are re-fetched every few seconds while a drill
//! is queued or running; the schedule form sits outside it so polling never resets a
//! choice being made.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::components::{
	BUTTON, LABEL, SELECT, Size, Status as Shape, Variant, button_class, card, card_content,
	card_section, empty_state, icon, table,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::pages::apps::chrome;
use crate::pages::databases::{database_url, when};
use crate::sse;
use crate::sse::js_string;
use crate::status::poll;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::backups::drills::schedule::Settings;
use podkit_core::backups::drills::service::{self as drills, Entry, Overview};
use podkit_core::backups::drills::{Drill, DrillStatus};
use podkit_core::error::ServerError;
use podkit_core::ids::{DatabaseId, ProjectId, TeamId};
use podkit_core::state::AppState;

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

/// The schedules the form offers; a custom cron set through the API is shown as is.
const PRESETS: [(&str, &str); 4] = [
	("", "Off"),
	("0 4 * * 0", "Weekly, Sunday 04:00"),
	("0 4 * * *", "Daily at 04:00"),
	("0 4 1 * *", "Monthly, on the 1st at 04:00"),
];

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{DB}{path}");
	Router::new()
		.route(&at("/backups/drills"), post(run_now))
		.route(&at("/backups/drills/list"), get(drills_poll))
		.route(&at("/backups/drills/schedule"), post(save_schedule))
}

fn drills_url(ctx: &TeamCtx, project: ProjectId, database: DatabaseId) -> String {
	format!(
		"{}/backups/drills",
		database_url(ctx.team.id, project, database)
	)
}

/// `850 ms`, `12.4 s`, `3 min 5 s`.
pub(super) fn human_duration(ms: i64) -> String {
	let ms = ms.max(0);
	if ms < 1000 {
		return format!("{ms} ms");
	}
	if ms < 60_000 {
		return format!("{}.{} s", ms / 1000, (ms % 1000) / 100);
	}
	let secs = ms / 1000;
	format!("{} min {} s", secs / 60, secs % 60)
}

const fn shape(status: DrillStatus) -> Shape {
	match status {
		DrillStatus::Queued => Shape::Pending,
		DrillStatus::Running => Shape::Building,
		DrillStatus::Passed => Shape::Running,
		DrillStatus::Failed => Shape::Failed,
	}
}

const fn label(status: DrillStatus) -> &'static str {
	match status {
		DrillStatus::Queued => "Queued",
		DrillStatus::Running => "Running",
		DrillStatus::Passed => "Passed",
		DrillStatus::Failed => "Failed",
	}
}

const fn in_flight(status: DrillStatus) -> bool {
	matches!(status, DrillStatus::Queued | DrillStatus::Running)
}

/// `12 of 12`, `12` without a source count, a dash before the count.
pub(super) fn tables(drill: &Drill) -> String {
	match (drill.tables_found, drill.source_tables) {
		(Some(found), Some(source)) => format!("{found} of {source}"),
		(Some(found), None) => found.to_string(),
		(None, _) => "\u{2014}".to_owned(),
	}
}

fn backup_used(drill: &Drill) -> String {
	drill
		.backup_taken_at
		.map_or_else(|| "\u{2014}".to_owned(), when)
}

fn trigger(entry: &Entry) -> String {
	match (&entry.triggered_by, entry.drill.triggered_by) {
		(Some(name), _) => format!("manual \u{b7} {name}"),
		(None, Some(_)) => "manual \u{b7} a removed user".to_owned(),
		(None, None) => "scheduled".to_owned(),
	}
}

fn status_cell(drill: &Drill) -> Markup {
	html! {
		div class="ui-stat-cell" {
			(chrome::dot(shape(drill.status), label(drill.status)))
			@if let Some(message) = &drill.error {
				p class="ui-cell-err" { (message) }
			}
		}
	}
}

/// The newest drill as facts: what happened, when, with which backup, what it found.
fn last_result(drill: &Drill) -> Markup {
	html! {
		dl class="ui-facts ui-facts-col" id="drill-last" {
			div { dt { "Last drill" } dd { (status_cell(drill)) } }
			div { dt { "Ran" } dd { (when(drill.started_at.unwrap_or(drill.created_at))) } }
			div { dt { "Backup used" } dd { (backup_used(drill)) } }
			div { dt { "Tables restored" } dd { (tables(drill)) } }
			@if let Some(ms) = drill.duration_ms {
				div { dt { "Took" } dd { (human_duration(ms)) } }
			}
		}
	}
}

/// The result and the history, patched whole after an action and while a drill runs.
fn section(ctx: &TeamCtx, base: &str, o: &Overview) -> Markup {
	let active = o.entries.iter().any(|e| in_flight(e.drill.status));
	let can_run = ctx.perms.can(Action::Create, Resource::Backup);
	let start = format!("@post({})", js_string(base));
	let action = html! {
		@if can_run {
			@match &o.can_run {
				Ok(()) => button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(start)
					"data-indicator:_drilling" "data-attr:disabled"={ "$_drilling || " (active) } {
					(icon("rotate-ccw")) "Run drill now"
				},
				Err(why) => p class="ui-hint ui-card-why" { (why) "." },
			}
		}
	};
	let rows: Vec<Vec<Markup>> = o
		.entries
		.iter()
		.map(|entry| {
			let d = &entry.drill;
			vec![
				html! { span class="whitespace-nowrap" { (when(d.created_at)) } },
				html! { (trigger(entry)) },
				html! { span class="whitespace-nowrap" { (backup_used(d)) } },
				html! { (tables(d)) },
				html! { span class="whitespace-nowrap" { (d.duration_ms.map_or_else(|| "\u{2014}".to_owned(), human_duration)) } },
				status_cell(d),
			]
		})
		.collect();
	html! {
		div id="drill-section" {
			(card(&html! {
				(card_section(
					"Restore drills",
					Some("A drill restores the newest backup into a throwaway copy on the same server, with no network, and counts its tables. This database is never touched."),
					Some(&action),
				))
				p class="text-sm text-destructive" role="alert" "data-show"="$_drill_error" "data-text"="$_drill_error" style="display: none" {}
				@if let Some(last) = o.entries.first() {
					(card_content(&html! {
						(last_result(&last.drill))
						(table("drill-table", "Restore drills of this database", &["Started", "Trigger", "Backup", "Tables", "Took", "Result"], &rows))
					}))
				} @else {
					(card_content(&empty_state("rotate-ccw", "No drills yet.", Some("A backup that was never restored is a guess. Run a drill now or schedule one."), None)))
				}
				(poll(&format!("{base}/list"), active))
			}))
		}
	}
}

fn next_run(settings: &Settings) -> Markup {
	html! {
		p id="drill-next" class="text-sm text-muted-foreground" role="status" {
			@match (&settings.schedule, settings.next_run_at) {
				(None, _) => "Scheduled drills are off.",
				(Some(_), Some(next)) => { "Next drill " (when(next)) "." },
				(Some(_), None) => "The schedule cannot be evaluated, so no drill is due.",
			}
		}
	}
}

/// The schedule: a choice of presets, saved on its own route.
fn schedule_card(ctx: &TeamCtx, base: &str, settings: &Settings) -> Markup {
	let current = settings.schedule.as_deref().unwrap_or("");
	let custom = !PRESETS.iter().any(|(cron, _)| *cron == current);
	if !ctx.perms.can(Action::Update, Resource::Database) {
		return card(&html! {
			(card_section("Drill schedule", None, None))
			(card_content(&html! {
				dl class="ui-facts ui-facts-col" {
					div { dt { "Schedule (cron, UTC)" } dd { code { (settings.schedule.as_deref().unwrap_or("Off")) } } }
				}
				(next_run(settings))
			}))
		});
	}
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/schedule"))
	);
	card(&html! {
		form id="drill-schedule" "data-on:submit"=(save) {
			(card_section("Drill schedule", Some("Off by default. Drills run in UTC and use the newest succeeded backup."), None))
			(card_content(&html! {
				label class=(LABEL) {
					"Run a drill"
					select class=(SELECT) "data-bind:drill_schedule" {
						button { selectedcontent {} }
						@for (cron, name) in PRESETS {
							option value=(cron) { (name) }
						}
						@if custom {
							option value=(current) { "Custom: " (current) }
						}
					}
				}
				(next_run(settings))
				button type="submit" class={ (BUTTON) " ui-block" } "data-indicator:_drill_saving" "data-attr:disabled"="$_drill_saving" {
					(icon("save")) "Save drill schedule"
				}
			}))
		}
	})
}

/// The whole drills block of the Backups tab. Its signals live on the wrapper, which no
/// action patches.
pub(super) fn block(
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
	o: &Overview,
) -> Markup {
	let base = drills_url(ctx, project, database);
	let signals = json!({
		"drill_schedule": o.settings.schedule.clone().unwrap_or_default(),
		"_drill_error": "",
	})
	.to_string();
	html! {
		div class="ui-two ui-two-wide" "data-signals"=(signals) {
			(section(ctx, &base, o))
			(schedule_card(ctx, &base, &o.settings))
		}
	}
}

pub(super) async fn overview_of(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Overview, ServerError> {
	drills::overview(&s.db, ctx.user.id, ctx.team.id, project, database).await
}

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	database: DatabaseId,
) -> Result<Markup, ServerError> {
	let o = overview_of(s, ctx, project, database).await?;
	Ok(section(ctx, &drills_url(ctx, project, database), &o))
}

async fn drills_poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match refreshed(&s, &ctx, project, database).await {
		Ok(section) => sse::patch(&section).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn run_now(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	let result = async {
		drills::run_now(&s, ctx.user.id, ctx.team.id, project, database).await?;
		refreshed(&s, &ctx, project, database).await
	}
	.await;
	match result {
		Ok(section) => sse::patch(&section)
			.and(sse::signals(&json!({ "_drill_error": "" })))
			.and(sse::toast(
				"Restore drill queued. The database itself is not touched.",
			))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_drill_error": error::message(&e) })).into_response(),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScheduleSignals {
	drill_schedule: String,
}

async fn save_schedule(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	ReadSignals(sig): ReadSignals<ScheduleSignals>,
) -> Response {
	let result = drills::set_schedule(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		database,
		Some(&sig.drill_schedule),
	)
	.await;
	match result {
		Ok(settings) => sse::patch(&next_run(&settings))
			.and(sse::signals(&json!({ "_drill_error": "" })))
			.and(sse::toast("Drill schedule saved"))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_drill_error": error::message(&e) })).into_response(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn durations_read_as_people_write_them() {
		for (ms, text) in [
			(0, "0 ms"),
			(850, "850 ms"),
			(1000, "1.0 s"),
			(12_449, "12.4 s"),
			(60_000, "1 min 0 s"),
			(185_900, "3 min 5 s"),
			(-5, "0 ms"),
		] {
			assert_eq!(human_duration(ms), text, "{ms}");
		}
	}

	#[test]
	fn presets_start_with_off() {
		assert_eq!(PRESETS[0], ("", "Off"));
		for (cron, _) in &PRESETS[1..] {
			assert!(podkit_core::backups::parse_schedule(cron).is_ok(), "{cron}");
		}
	}
}
