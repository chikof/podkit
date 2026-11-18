//! The Deployments tab: a timeline of every attempt to build and run the application, one
//! deployment's page with its live build log, and the actions on them (Rollback, Cancel).
//!
//! Rollback and Cancel follow the same rules the buttons do, so a forged post for a running,
//! in-flight or imageless deployment is refused with a reason. Both come back as SSE; a refusal
//! lands in the `_action_error` signal that the header and the rollback dialog show.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde_json::json;
use time::OffsetDateTime;

use super::header::{action_error, deployment_url, fail, header};
use super::{Tab, app_url, load, page, recent_deployments};
use crate::applications::service::{self as apps, ApplicationView};
use crate::auth::permission::{Action, Resource};
use crate::deployments::service::{self, DeploymentView, Trigger};
use crate::error::ServerError;
use crate::ids::{ApplicationId, DeploymentId, ProjectId, TeamId};
use crate::projects::overview::ResourceStatus;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, Size, Tone, Variant, badge, button_class, card, card_content, card_section,
	empty_state, icon, log_viewer,
};
use crate::ui::extract::TeamCtx;
use crate::ui::live::job_log;
use crate::ui::pages::projects::cards::{dot_class, relative_time, status_line};
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll};
use crate::ui::{error, sse};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{APP}{path}");
	Router::new()
		.route(&at("/deployments"), get(history))
		.route(&at("/deployments/table"), get(table_fragment))
		.route(&at("/deployments/{deployment_id}"), get(detail))
		.route(
			&at("/deployments/{deployment_id}/status"),
			get(status_fragment),
		)
		.route(&at("/deployments/{deployment_id}/log"), get(build_log))
		.route(&at("/deployments/{deployment_id}/rollback"), post(rollback))
		.route(&at("/deployments/{deployment_id}/cancel"), post(cancel))
}

// --- small formatters -------------------------------------------------------------------------

fn two(n: u8) -> String {
	format!("{n:02}")
}

/// `2026-11-17 20:03 UTC`: no locale, no JS, same everywhere.
fn when(t: OffsetDateTime) -> String {
	let t = t.to_offset(time::UtcOffset::UTC);
	format!(
		"{}-{}-{} {}:{} UTC",
		t.date().year(),
		two(t.month() as u8),
		two(t.day()),
		two(t.hour()),
		two(t.minute()),
	)
}

fn duration(d: time::Duration) -> String {
	let secs = d.whole_seconds().max(0);
	match (secs / 3600, secs / 60 % 60, secs % 60) {
		(0, 0, s) => format!("{s}s"),
		(0, m, s) => format!("{m}m {s}s"),
		(h, m, _) => format!("{h}h {m}m"),
	}
}

fn short_sha(sha: &str) -> &str {
	sha.get(..7).unwrap_or(sha)
}

fn trigger_label(t: &Trigger) -> &'static str {
	match t {
		Trigger::User => "User",
		Trigger::Webhook => "Webhook",
	}
}

// --- the timeline -----------------------------------------------------------------------------

/// The button that opens the rollback dialog for deployment `id`.
fn rollback_button(id: DeploymentId) -> Markup {
	let fallback = "el.commandForElement || document.getElementById('rollback-dialog').showModal()";
	html! {
		button type="button" class=(button_class(Variant::Outline, Size::Sm)) command="show-modal" commandfor="rollback-dialog"
			aria-haspopup="dialog" "data-on:click"={ "$_action_error = ''; $_rb = '" (id) "'; " (fallback) } {
			"Rollback"
		}
	}
}

fn timeline_row(
	base: &str,
	d: &DeploymentView,
	now: OffsetDateTime,
	can_create: bool,
	can_update: bool,
) -> Markup {
	let url = deployment_url(base, d.id);
	html! {
		li class="ui-timeline-item" {
			span class="ui-timeline-node" aria-hidden="true" { span class=(dot_class(d.status.tone())) {} }
			div class="flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-2" {
				div class="flex min-w-0 flex-col gap-1" {
					div class="flex flex-wrap items-center gap-x-3 gap-y-1" {
						a class="text-sm font-medium hover:underline" href=(url) { (d.status.label()) }
						span class="font-mono text-xs text-muted-foreground" { "#" (d.id) }
						(badge(Tone::Neutral, trigger_label(&d.trigger)))
					}
					p class="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground" {
						@if let Some(sha) = &d.commit_sha { code class="font-mono" { (short_sha(sha)) } }
						(relative_time(d.created_at, now))
						@if let Some(t) = d.duration() { span { (duration(t)) } }
					}
				}
				div class="flex items-center gap-2" {
					a class=(button_class(Variant::Ghost, Size::Sm)) href=(url) { "View log" }
					@if can_update && d.can_cancel() {
						button type="button" class=(button_class(Variant::Outline, Size::Sm))
							"data-on:click"={ "@post(" (js_string(&format!("{url}/cancel"))) ")" } { "Cancel" }
					}
					@if can_create && d.can_rollback() { (rollback_button(d.id)) }
				}
			}
		}
	}
}

/// The history. `#deployments` is what polling and actions re-render; the polling element is
/// part of it, so it disappears once nothing is in flight.
fn table(ctx: &TeamCtx, app: &ApplicationView, rows: &[DeploymentView]) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Deployment);
	let can_update = ctx.perms.can(Action::Update, Resource::Deployment);
	let active = rows.iter().any(DeploymentView::in_flight);
	let now = OffsetDateTime::now_utc();
	html! {
		div id="deployments" {
			(poll(&format!("{base}/deployments/table"), active))
			@if rows.is_empty() {
				(empty_state("rocket", "Nothing has been deployed yet.", Some("Deploy it and every attempt shows up here, with its log."), None))
			} @else {
				ol class="ui-timeline" aria-label="Deployments, newest first" {
					@for d in rows { (timeline_row(&base, d, now, can_create, can_update)) }
				}
			}
		}
	}
}

async fn history_rows(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Result<Vec<DeploymentView>, ServerError> {
	service::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
}

async fn history(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let rows = history_rows(&s, &ctx, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let content = card(&html! {
		(card_section("Deployments", Some("Every attempt to build and run this application, newest first."), None))
		(card_content(&table(&ctx, &app, &rows)))
	});
	Ok(page(&s, &ctx, &project, &app, Tab::Deployments, &content).await)
}

/// What polling fetches: the history again, with its polling element only while something is
/// still in flight, and the header (its status and buttons follow the newest deployment).
async fn table_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	let result = async {
		let view = apps::get(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		let rows = history_rows(&s, &ctx, project, app).await?;
		Ok::<_, ServerError>((view, rows))
	}
	.await;
	match result {
		Ok((view, rows)) => sse::patch(&table(&ctx, &view, &rows))
			.and(sse::patch(&header(&s, &ctx, &view, &rows)))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn rollback(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	let result = async {
		// The same rule the buttons follow, so a forged post for a running, in-flight or
		// imageless deployment is refused with a reason.
		let target = service::get(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		if !target.has_image {
			return Err(ServerError::Validation(
				"That deployment never produced an image to roll back to.".to_owned(),
			));
		}
		if !target.can_rollback() {
			return Err(ServerError::Validation(
				"That deployment is live or still in progress, so there is nothing to roll back to."
					.to_owned(),
			));
		}
		service::rollback(&s, ctx.user.id, ctx.team.id, project, app, id).await
	}
	.await;
	match result {
		Ok(d) => sse::redirect(&deployment_url(&app_url(ctx.team.id, project, app), d.id))
			.into_response(),
		Err(e) => fail(&e),
	}
}

async fn cancel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	match service::cancel(&s, ctx.user.id, ctx.team.id, project, app, id).await {
		// The worker moves the row to cancelled a moment later; polling shows it.
		Ok(_) => super::toast_ok("Cancelling")
			.and(action_error(""))
			.into_response(),
		Err(e) => fail(&e),
	}
}

// --- one deployment ---------------------------------------------------------------------------

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			dt class="text-muted-foreground" { (label) }
			dd class="font-medium break-words" { (value) }
		}
	}
}

/// Status, times and the Cancel button. `#deployment-status` is what polling re-renders while
/// the deployment is in flight.
fn status_card(ctx: &TeamCtx, app: &ApplicationView, d: &DeploymentView) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let url = deployment_url(&base, d.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Deployment);
	html! {
		div id="deployment-status" {
			(poll(&format!("{url}/status"), d.in_flight()))
			(card(&html! {
				(card_section(&format!("Deployment #{}", d.id), None, None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-3" {
							(fact("Status", &status_line(ResourceStatus::Deployment(d.status))))
							(fact("Trigger", &html! { (trigger_label(&d.trigger)) }))
							(fact("Commit", &html! {
								@if let Some(sha) = &d.commit_sha { code class="font-mono text-xs" { (short_sha(sha)) } } @else { "Not cloned yet" }
							}))
							(fact("Created", &html! { (when(d.created_at)) }))
							(fact("Started", &html! {
								@if let Some(t) = d.started_at { (when(t)) } @else { "-" }
							}))
							(fact("Duration", &html! {
								@if let Some(t) = d.duration() { (duration(t)) } @else { "-" }
							}))
						}
						@if let Some(message) = &d.error_message {
							p class="rounded-lg border border-destructive/30 bg-destructive/15 p-3 text-sm break-words text-destructive" role="alert" { (message) }
						}
						@if can_update && d.can_cancel() {
							div {
								button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post(" (js_string(&format!("{url}/cancel"))) ")" } { "Cancel deployment" }
							}
						}
					}
				}))
			}))
		}
	}
}

async fn detail(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let d = service::get(&s.db, ctx.user.id, ctx.team.id, project.id, app.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let url = deployment_url(&base, d.id);
	let content = html! {
		a href={ (base) "/deployments" } class="inline-flex w-fit items-center gap-1 text-sm text-muted-foreground hover:text-foreground hover:underline" {
			(icon("arrow-left")) "All deployments"
		}
		(status_card(&ctx, &app, &d))
		(card(&html! {
			(card_section("Build log", Some("Clone, build and start output."), None))
			(card_content(&log_viewer("build-log", "Build log", &format!("{url}/log"))))
		}))
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Deployments, &content).await)
}

async fn status_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	let result = async {
		let view = apps::get(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		let d = service::get(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		Ok::<_, ServerError>((view, d))
	}
	.await;
	match result {
		// The header's status and buttons depend on the latest status too.
		Ok((view, d)) => {
			let rows = recent_deployments(&s, &ctx, &view).await;
			sse::patch(&status_card(&ctx, &view, &d))
				.and(sse::patch(&header(&s, &ctx, &view, &rows)))
				.into_response()
		}
		Err(e) => fail(&e),
	}
}

/// Why a stream could not start. Not found and not allowed are plain status codes, like any
/// other page; anything else (nothing running, the agent down) is written into the log itself.
pub(super) fn stream_failure(e: &ServerError, target: &str) -> Response {
	let status = e.status();
	if status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN {
		return status.into_response();
	}
	let line = html! { div class="break-all whitespace-pre-wrap text-destructive" { (error::message(e)) } };
	sse::patch_into(&format!("#{target}"), ElementPatchMode::Append, &line)
		.and(sse::signals(&json!({ "_live": false })))
		.into_response()
}

async fn build_log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Response {
	match service::build_log_job(&s.db, ctx.user.id, ctx.team.id, project, app, id).await {
		Ok(job) => job_log(&s, job, "build-log"),
		Err(e) => stream_failure(&e, "build-log"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn durations_and_times_read_plainly() {
		assert_eq!(duration(time::Duration::seconds(5)), "5s");
		assert_eq!(duration(time::Duration::seconds(75)), "1m 15s");
		assert_eq!(duration(time::Duration::seconds(3700)), "1h 1m");
		assert_eq!(duration(time::Duration::seconds(-3)), "0s");
		let t = time::macros::datetime!(2026-11-17 20:03:59 UTC);
		assert_eq!(when(t), "2026-11-17 20:03 UTC");
		assert_eq!(short_sha("abcdef1234"), "abcdef1");
		assert_eq!(short_sha("abc"), "abc");
	}
}
