//! The General tab: what the database is, its port and resource limits, and the log of the
//! latest job. Saving the port or a limit recreates the container (its data is kept); the
//! same save also stores the backup settings, which the Backups tab edits, so the page's
//! signals carry them all (see `frame_signals`).

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use super::header::reloaded;
use super::{Tab, database_url, engine_label, id, lenient, load, number, page, when};
use crate::auth::permission::{Action, Resource};
use crate::databases::service::{self, SettingsInput};
use crate::error::ServerError;
use crate::ids::{BackupTargetId, DatabaseId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	INPUT, LABEL, card, card_content, card_section, empty_state, log_viewer, save_bar,
	save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::live::job_log;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}",
			get(general_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/job-log",
			get(job_log_stream),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/settings",
			post(save),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/{database_id}/schedule",
			post(schedule_preview),
		)
}

async fn general_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	let (project, d) = load(&s, &ctx, project, database).await?;
	let database = &d.database;
	let base = database_url(ctx.team.id, project.id, database.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Database);
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/settings"))
	);

	let content = html! {
		div class="flex flex-col gap-6" {
			div class="ui-two" {
			(card(&html! {
				(card_section("Details", None, None))
				(card_content(&html! {
					dl class="ui-facts ui-facts-col" {
						(fact("Engine", engine_label(database.engine)))
						(fact("Version", &database.version))
						(fact("Server", &d.server_name))
						(fact("Public port", &database.public_port.map_or_else(|| "Not published".to_owned(), |p| p.to_string())))
						(fact("Memory limit", &database.memory_limit_mb.map_or_else(|| "No limit".to_owned(), |m| format!("{m} MB"))))
						(fact("CPU limit", &database.cpu_limit.map_or_else(|| "No limit".to_owned(), |c| format!("{c} cores"))))
					}
				}))
			}))
			@if can_update {
				form "data-on:submit"=(save) {
					(card(&html! {
						(card_section("Network and limits", Some("Changing the public port or a limit recreates the container; its data is kept."), None))
						(card_content(&html! {
							label class=(LABEL) {
								"Public port"
								input class=(INPUT) type="text" inputmode="numeric" placeholder="Not published" "data-bind:public_port";
								span class="ui-hint" { "Publishes the database on the server's host, 1024 to 65535. Leave empty to keep it reachable only by your applications." }
							}
							div class="ui-pair" {
								label class=(LABEL) {
									"Memory limit in MB"
									input class=(INPUT) type="text" inputmode="numeric" placeholder="No limit" "data-bind:memory_limit_mb";
								}
								label class=(LABEL) {
									"CPU limit in cores"
									input class=(INPUT) type="text" inputmode="decimal" placeholder="No limit" "data-bind:cpu_limit";
								}
							}
						}))
					}))
					(save_bar(&["public_port", "memory_limit_mb", "cpu_limit"]))
				}
			}
			}
			(card(&html! {
				(card_section("Latest job", Some("What podkit did most recently to this database."), None))
				(card_content(&html! {
					@if d.job.is_some() {
						(log_viewer("job-log", "Job log", &format!("{base}/job-log")))
					} @else {
						(empty_state("scroll-text", "No jobs have run yet.", Some("Provisioning, starting and backing up show their output here."), None))
					}
				}))
			}))
		}
	};
	Ok(page(&s, &ctx, &project, &d, Tab::General, &content).await)
}

fn fact(label: &str, value: &str) -> Markup {
	html! {
		div {
			dt { (label) }
			dd { (value) }
		}
	}
}

async fn job_log_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Response {
	match service::latest_job(&s.db, ctx.user.id, ctx.team.id, project, database).await {
		Ok(job) => job_log(&s, job, "job-log"),
		Err(e) => log_failure("job-log", &e),
	}
}

/// A log stream that could not start: say so inside the log and stop the "Live" indicator.
pub(super) fn log_failure(target: &str, e: &ServerError) -> Response {
	let message = error::message(e);
	let line = html! {
		div class="break-all whitespace-pre-wrap text-muted-foreground" { (message) }
	};
	sse::patch_into(&format!("#{target}"), ElementPatchMode::Append, &line)
		.and(sse::signals(&json!({ "_live": false })))
		.into_response()
}

// --- settings -----------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
pub(in crate::ui) struct SettingsSignals {
	#[serde(deserialize_with = "lenient")]
	public_port: String,
	#[serde(deserialize_with = "lenient")]
	memory_limit_mb: String,
	#[serde(deserialize_with = "lenient")]
	cpu_limit: String,
	#[serde(deserialize_with = "lenient")]
	backup_target_id: String,
	backup_schedule: String,
	#[serde(deserialize_with = "lenient")]
	backup_keep: String,
}

impl SettingsSignals {
	pub(in crate::ui) fn schedule(&self) -> Option<&str> {
		let schedule = self.backup_schedule.trim();
		(!schedule.is_empty()).then_some(schedule)
	}

	pub(in crate::ui) fn input(&self) -> Result<SettingsInput, ServerError> {
		Ok(SettingsInput {
			public_port: number(&self.public_port, "Public port")?,
			memory_limit_mb: number(&self.memory_limit_mb, "Memory limit")?,
			cpu_limit: number(&self.cpu_limit, "CPU limit")?,
			backup_target_id: id(&self.backup_target_id, "backup target", BackupTargetId)?,
			backup_schedule: self.schedule().map(str::to_owned),
			backup_keep: number(&self.backup_keep, "Backups to keep")?,
		})
	}
}

/// What the schedule field says under it: nothing set, the next three runs, or why the
/// expression is not valid.
pub(in crate::ui) fn schedule_hint(
	schedule: Option<&Result<Vec<OffsetDateTime>, String>>,
) -> Markup {
	html! {
		div id="schedule-hint" class="text-sm font-normal" role="status" {
			@match schedule {
				None => p class="text-muted-foreground" { "No schedule: backups run only when you start them." },
				Some(Ok(runs)) => {
					p class="text-muted-foreground" { "Next runs" }
					ol class="mt-1 flex flex-col gap-0.5 font-mono text-xs text-muted-foreground" {
						@for run in runs { li { (when(*run)) } }
					}
				},
				Some(Err(why)) => p class="text-destructive" { "Not a valid schedule: " (why) },
			}
		}
	}
}

async fn schedule_preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	ReadSignals(sig): ReadSignals<SettingsSignals>,
) -> Response {
	let Some(schedule) = sig.schedule() else {
		return sse::patch(&schedule_hint(None)).into_response();
	};
	match service::preview_schedule(&s.db, ctx.user.id, ctx.team.id, project, database, schedule)
		.await
	{
		Ok(result) => sse::patch(&schedule_hint(Some(&result))).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn save(
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
			.map(|schedule| service::upcoming_runs(schedule, service::PREVIEW_RUNS));
		if let Some(Err(_)) = &runs {
			return Ok(Err(schedule_hint(runs.as_ref())));
		}
		let saved =
			service::update_settings(&s, ctx.user.id, ctx.team.id, project, database, input)
				.await?;
		Ok::<_, ServerError>(Ok((saved, runs)))
	}
	.await;
	match result {
		Ok(Err(hint)) => sse::patch(&hint)
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response(),
		Ok(Ok(((updated, true), _))) => reloaded(&ctx, &updated),
		Ok(Ok(((_, false), runs))) => sse::patch(&schedule_hint(runs.as_ref()))
			.and(sse::signals(&json!({ "_error": "" })))
			.and(sse::toast("Settings saved"))
			.and(save_bar_saved())
			.into_response(),
		Err(e) => error::action(&e),
	}
}
