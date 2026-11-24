//! The application's own page and its Deployments tab: a timeline of every attempt to build
//! and run the application, with the log of the one being looked at beside it (the newest
//! unless a deployment's link was followed), and the actions on them (Rollback, Cancel).
//!
//! Rollback and Cancel follow the same rules the buttons do, so a forged post for a running,
//! in-flight or imageless deployment is refused with a reason. Both come back as SSE; a refusal
//! lands in the `_action_error` signal that the header and the rollback dialog show.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde_json::json;
use time::OffsetDateTime;

use super::chrome;
use super::header::{action_error, deployment_url, fail, header};
use super::{Tab, app_url, load, page, recent_deployments};
use crate::components::{
	BUTTON_OUTLINE, BannerKind, Size, Variant, banner, button_class, empty_state, icon,
	log_viewer_titled,
};
use crate::extract::TeamCtx;
use crate::live::{error_line, job_log};
use crate::pages::projects::cards::relative_time;
use crate::pages::security;
use crate::sse::js_string;
use crate::status::poll;
use crate::{error, sse};
use podkit_core::applications::SourceKind;
use podkit_core::applications::service::{self as apps, ApplicationView};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::deployments::service::{self, DeploymentView, Trigger};
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, DeploymentId, ProjectId, TeamId};
use podkit_core::scanning::service::ScanView;
use podkit_core::state::AppState;

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{APP}{path}");
	Router::new()
		.route(&at(""), get(overview))
		.route(&at("/deployments"), get(overview))
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
		Trigger::User => "Manual",
		Trigger::Webhook => "Webhook",
		Trigger::AutoRollback => "Automatic rollback",
	}
}

/// What a deployment says about itself in the timeline.
fn title_of(d: &DeploymentView) -> String {
	if let Some(origin) = &d.promoted_from {
		return format!("Promoted from {}", origin.environment);
	}
	match d.trigger {
		Trigger::User => "Deployed by hand",
		Trigger::Webhook => "Deployed by webhook",
		Trigger::AutoRollback => "Rolled back automatically",
	}
	.to_owned()
}

/// Where a promoted deployment came from: the source deployment, linked while it exists, and
/// its environment. Nothing for any other deployment.
fn origin(team: TeamId, project: ProjectId, d: &DeploymentView) -> Markup {
	let Some(origin) = &d.promoted_from else {
		return html! {};
	};
	html! {
		@if let Some((app, id)) = origin.deployment {
			a href=(deployment_url(&app_url(team, project, app), id)) { "#" (id) } " in " (origin.environment)
		} @else {
			(origin.environment) " (since deleted)"
		}
	}
}

// --- the timeline -----------------------------------------------------------------------------

/// One line under a row when the health gate has something to say: why a release was put
/// back, what a rollback replaced, or that the deploy kept the old container.
fn health_note(d: &DeploymentView) -> Option<String> {
	use podkit_core::health_gate::State;
	if let Some(replaced) = d.rollback_of {
		return Some(format!("Replaced #{replaced}, which went bad"));
	}
	let gate = d.gate.as_ref()?;
	match gate.state {
		State::RolledBack => Some(format!(
			"Rolled back: {}",
			gate.detail
				.as_deref()
				.unwrap_or("it failed its health check")
		)),
		State::Unhealthy => {
			Some("Never became healthy; the previous container kept serving".to_owned())
		}
		State::Waiting | State::Healthy => None,
	}
}

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
	(team, project): (TeamId, ProjectId),
	base: &str,
	d: &DeploymentView,
	selected: bool,
	now: OffsetDateTime,
	can_create: bool,
	can_update: bool,
) -> Markup {
	let url = deployment_url(base, d.id);
	let (shape, state) = chrome::deployment_state(d);
	let note = health_note(d);
	html! {
		li class="ui-timeline-item" data-status=(shape.key()) data-current=[selected.then_some("")] {
			span class="ui-timeline-node" data-status=(shape.key()) aria-hidden="true" {
				span class="ui-dot" data-status=(shape.key()) {}
			}
			div class="ui-tl-body" {
				p class="ui-tl-title" {
					a href=(url) aria-current=[selected.then_some("true")] {
						b { "#" (d.id) } " " (title_of(d))
					}
				}
				@if let Some(sha) = &d.commit_sha {
					p class="ui-tl-meta" { code { (short_sha(sha)) } }
				}
				@if d.promoted_from.is_some() {
					p class="ui-tl-meta" { "From " (origin(team, project, d)) }
				}
				@if let Some(note) = &note {
					p class="ui-tl-meta" { (note) }
				}
				p class="ui-tl-meta2" {
					span class="ui-tl-state" { (state) }
					@if let Some(t) = d.duration() { span { (duration(t)) } }
					(relative_time(d.created_at, now))
				}
				@if (can_update && d.can_cancel()) || (can_create && d.can_rollback()) {
					div class="ui-tl-actions" {
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
}

/// The history. `#deployments` is what polling and actions re-render; the polling element is
/// part of it, so it disappears once nothing is in flight.
fn table(
	ctx: &TeamCtx,
	app: &ApplicationView,
	rows: &[DeploymentView],
	selected: Option<DeploymentId>,
) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Deployment);
	let can_update = ctx.perms.can(Action::Update, Resource::Deployment);
	let active = rows.iter().any(DeploymentView::in_flight);
	let now = OffsetDateTime::now_utc();
	let poll_url = match selected {
		Some(id) => format!("{base}/deployments/table?current={id}"),
		None => format!("{base}/deployments/table"),
	};
	html! {
		div id="deployments" {
			(poll(&poll_url, active))
			ol class="ui-timeline" aria-label="Deployments, newest first" {
				@for d in rows { (timeline_row((ctx.team.id, app.project_id), &base, d, selected == Some(d.id), now, can_create, can_update)) }
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

/// The scans of these deployments, for the line each deploy page shows. Informational: a
/// failed lookup is an empty answer, never a failed page.
async fn scans_of(
	s: &AppState,
	ctx: &TeamCtx,
	rows: &[DeploymentView],
) -> security::DeploymentScans {
	let ids: Vec<DeploymentId> = rows.iter().map(|d| d.id).collect();
	security::deployment_scans(s, ctx, &ids).await
}

/// What the page shows: the history, and the facts and log of `selected` (the newest when
/// nothing was chosen).
fn view(
	ctx: &TeamCtx,
	app: &ApplicationView,
	rows: &[DeploymentView],
	chosen: Option<DeploymentId>,
	scans: &security::DeploymentScans,
) -> Markup {
	let current = chosen
		.and_then(|id| rows.iter().find(|d| d.id == id))
		.or_else(|| rows.first());
	let base = app_url(ctx.team.id, app.project_id, app.id);
	html! {
		@if let Some(d) = current {
			div class="ui-dep" {
				section aria-labelledby="deployments-title" {
					h2 id="deployments-title" class="ui-dep-h" { "Deployments" }
					(table(ctx, app, rows, Some(d.id)))
				}
				div class="ui-dep-main" {
					(status_card(ctx, app, d, scans.get(&d.id)))
					(log_viewer_titled(
						"build-log",
						"Build log",
						&format!("{}/log", deployment_url(&base, d.id)),
						Some(&html! { "Build log " span class="ui-logbar-id" { "#" (d.id) } }),
					))
				}
			}
		} @else {
			(empty_state("rocket", "Nothing has been deployed yet.", Some("Deploy it and every attempt shows up here, with its log."), None))
		}
	}
}

/// The application's page, and the Deployments tab on its own path: the newest deployment.
async fn overview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	// Someone who may not read deployments lands on the settings instead of a refusal.
	if !Tab::Deployments.allowed(&ctx) {
		let to = format!("{}/settings", app_url(ctx.team.id, project.id, app.id));
		return Err(Redirect::to(&to).into_response());
	}
	let rows = history_rows(&s, &ctx, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let scans = scans_of(&s, &ctx, &rows).await;
	let content = view(&ctx, &app, &rows, None, &scans);
	Ok(page(&s, &ctx, &project, &app, Tab::Deployments, &content).await)
}

/// One deployment chosen from the timeline: the same page with its log.
async fn detail(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DeploymentId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	// Unknown here is a real 404 (and another team's the same), not a quiet fallback.
	service::get(&s.db, ctx.user.id, ctx.team.id, project.id, app.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let rows = history_rows(&s, &ctx, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let scans = scans_of(&s, &ctx, &rows).await;
	let content = view(&ctx, &app, &rows, Some(id), &scans);
	Ok(page(&s, &ctx, &project, &app, Tab::Deployments, &content).await)
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Current {
	current: Option<i64>,
}

/// What polling fetches: the history again, with its polling element only while something is
/// still in flight, and the header (its status and buttons follow the newest deployment).
/// `current` is the deployment being read, so its mark stays put while a newer one builds.
async fn table_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	axum::extract::Query(q): axum::extract::Query<Current>,
) -> Response {
	let result = async {
		let view = apps::get(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		let rows = history_rows(&s, &ctx, project, app).await?;
		Ok::<_, ServerError>((view, rows))
	}
	.await;
	match result {
		Ok((view, rows)) => {
			let selected = q
				.current
				.and_then(|id| rows.iter().find(|d| d.id.0 == id))
				.or_else(|| rows.first())
				.map(|d| d.id);
			sse::patch(&table(&ctx, &view, &rows, selected))
				.and(sse::patch(&header(&s, &ctx, &view, &rows).await))
				.into_response()
		}
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
		div {
			dt { (label) }
			dd { (value) }
		}
	}
}

/// Status, times and the Cancel button of the deployment being read. `#deployment-status` is
/// what polling re-renders while it is in flight.
fn status_card(
	ctx: &TeamCtx,
	app: &ApplicationView,
	d: &DeploymentView,
	scan: Option<&ScanView>,
) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let url = deployment_url(&base, d.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Deployment);
	html! {
		div id="deployment-status" class="flex flex-col gap-3" {
			(poll(&format!("{url}/status"), d.in_flight()))
			dl class="ui-facts ui-facts-row" aria-label={ "Deployment #" (d.id) } {
				(fact("Status", &{
					let (shape, word) = chrome::deployment_state(d);
					chrome::dot(shape, &word)
				}))
				(fact("Trigger", &html! { (trigger_label(&d.trigger)) }))
				@if d.promoted_from.is_some() {
					(fact("Promoted from", &origin(ctx.team.id, app.project_id, d)))
				}
				@if let Some(note) = health_note(d) {
					(fact("Health", &html! { (note) }))
				}
				// An image application is never cloned: there is no commit to name.
				@if app.source_kind == SourceKind::Git {
					(fact("Commit", &html! {
						@if let Some(sha) = &d.commit_sha { code { (short_sha(sha)) } } @else { "Not cloned yet" }
					}))
				}
				(fact("Started", &html! {
					@if let Some(t) = d.started_at { (when(t)) } @else { "\u{2014}" }
				}))
				(fact("Duration", &html! {
					@if let Some(t) = d.duration() { (duration(t)) } @else { "\u{2014}" }
				}))
				@if can_update && d.can_cancel() {
					div class="ui-facts-act" {
						button type="button" class=(BUTTON_OUTLINE) "data-on:click"={ "@post(" (js_string(&format!("{url}/cancel"))) ")" } {
							(icon("square")) "Cancel deployment"
						}
					}
				}
			}
			@if let Some(message) = &d.error_message {
				(banner(BannerKind::Error, Some("This deployment failed."), message, None))
			}
			(security::scan_line(ctx, d.id, scan))
		}
	}
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
			let scans = security::deployment_scans(&s, &ctx, &[d.id]).await;
			sse::patch(&status_card(&ctx, &view, &d, scans.get(&d.id)))
				.and(sse::patch(&header(&s, &ctx, &view, &rows).await))
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
	let line = error_line(&error::message(e));
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
