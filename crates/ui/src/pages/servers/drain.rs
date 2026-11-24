//! Drain a server (`moves`): the Overview's "Drain server" opens a dialog that lists what is
//! on the server and what a drain into the picked server would do with each thing (move it,
//! and how; or keep it, and why). Draining marks the server, so nothing new is put on it, and
//! moves the applications one at a time; the Overview then shows the drain's report, asking
//! for it again while it runs, and "Stop draining" takes the mark off.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use time::OffsetDateTime;

use super::detail_url;
use crate::components::{
	BUTTON, BUTTON_OUTLINE, SELECT, Tone, badge, dialog, dialog_open_button, icon,
};
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::sse::js_string;
use crate::status::poll;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{BackupTargetId, ServerId, TeamId};
use podkit_core::moves::service::{self, DrainPlan, Found};
use podkit_core::moves::{Drain, DrainStatus, Item, ItemKind, Outcome};
use podkit_core::servers::{Server, ServerStatus};
use podkit_core::state::AppState;

/// The id of the dialog.
pub const DIALOG: &str = "drain-server";
const BODY: &str = "drain-body";
const SECTION: &str = "server-drain";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/servers/{server_id}/drain/plan",
			get(plan_body),
		)
		.route(
			"/teams/{team_id}/servers/{server_id}/drain/section",
			get(section_fragment),
		)
		.route("/teams/{team_id}/servers/{server_id}/drain", post(start))
		.route("/teams/{team_id}/servers/{server_id}/undrain", post(stop))
}

/// Whether the person may drain servers: `update server`.
#[must_use]
pub fn allowed(ctx: &TeamCtx) -> bool {
	ctx.perms.can(Action::Update, Resource::Server)
}

fn outcome_badge(outcome: Outcome) -> Markup {
	let (tone, text) = match outcome {
		Outcome::Pending => (Tone::Neutral, "to move"),
		Outcome::Moving => (Tone::Progress, "moving"),
		Outcome::Moved => (Tone::Success, "moved"),
		Outcome::Failed => (Tone::Danger, "failed"),
		Outcome::Kept => (Tone::Warning, "stays"),
		Outcome::Skipped => (Tone::Neutral, "skipped"),
	};
	badge(tone, text)
}

fn drain_badge(status: DrainStatus) -> Markup {
	let tone = match status {
		DrainStatus::Queued | DrainStatus::Running => Tone::Progress,
		DrainStatus::Succeeded => Tone::Success,
		DrainStatus::Partial => Tone::Warning,
		DrainStatus::Failed => Tone::Danger,
		DrainStatus::Cancelled => Tone::Neutral,
	};
	badge(tone, status.as_str())
}

fn when(t: OffsetDateTime) -> String {
	format!(
		"{:04}-{:02}-{:02} {:02}:{:02} UTC",
		t.year(),
		u8::from(t.month()),
		t.day(),
		t.hour(),
		t.minute()
	)
}

/// One thing of a report: what it is, where it belongs, what became of it and why.
fn item_row(item: &Item, extra: Option<&Markup>) -> Markup {
	html! {
		li class="flex flex-col gap-1 rounded-lg border border-border px-4 py-3" {
			div class="flex flex-wrap items-center justify-between gap-3" {
				span class="text-sm" {
					span class="text-muted-foreground" { (item.kind.label()) " " }
					span class="font-medium" { (item.name) }
					@if let Some(project) = &item.project {
						span class="text-muted-foreground" { " \u{b7} " (project) }
					}
				}
				(outcome_badge(item.outcome))
			}
			@if let Some(method) = item.method.filter(|_| item.kind == ItemKind::Application && item.outcome != Outcome::Kept) {
				span class="text-xs text-muted-foreground" { (method.label()) }
			}
			@if let Some(detail) = &item.detail {
				span class={ "text-xs " @if matches!(item.outcome, Outcome::Failed) { "text-destructive" } @else { "text-muted-foreground" } } {
					(detail)
				}
			}
			@if let Some(extra) = extra { (extra) }
		}
	}
}

/// The Overview's drain section: the mark, and the newest drain's report. Empty for a server
/// that was never drained and is not draining.
#[must_use]
pub fn section(
	ctx: &TeamCtx,
	server: &Server,
	draining_since: Option<OffsetDateTime>,
	latest: Option<&Drain>,
) -> Markup {
	let base = detail_url(ctx.team.id, server.id);
	let running = latest.is_some_and(|d| d.status.in_flight());
	let undrain = format!("@post({})", js_string(&format!("{base}/undrain")));
	html! {
		section id=(SECTION) class="ui-stack" aria-labelledby="drain-title" {
			@if draining_since.is_some() || latest.is_some() {
				h2 id="drain-title" class="ui-sec-h" { "Drain" }
				@if let Some(since) = draining_since {
					div class="flex flex-wrap items-center justify-between gap-3" {
						p class="ui-lede" {
							(badge(Tone::Warning, "Draining")) " since " (when(since))
							". Nothing new is put on this server: it is left out of the create forms, and builds go elsewhere."
						}
						@if allowed(ctx) {
							button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(undrain)
								"data-indicator:_busy" "data-attr:disabled"="$_busy" {
								(icon("x")) @if running { "Stop the drain" } @else { "Stop draining" }
							}
						}
					}
				}
				@if let Some(d) = latest {
					div class="flex flex-wrap items-center gap-2" {
						span class="text-sm" { "Into " strong { (d.target_server_name) } }
						(drain_badge(d.status))
						span class="text-xs text-muted-foreground" { "asked " (when(d.created_at)) }
					}
					@if d.report.0.is_empty() {
						p class="ui-lede" { "Nothing was on the server." }
					} @else {
						ul class="flex flex-col gap-2" aria-label="What became of each thing" {
							@for item in &d.report.0 { (item_row(item, None)) }
						}
					}
				}
				(poll(&format!("{base}/drain/section"), running))
			}
		}
	}
}

/// What a drain would do with one thing found on the server.
fn found_row(f: &Found) -> Markup {
	let warnings = f.verdict.as_ref().filter(|v| v.movable()).map(|v| {
		html! {
			span class="text-xs text-muted-foreground" { "Downtime: " (v.downtime) }
			@for w in &v.warnings {
				span class="text-xs text-muted-foreground" { (w) }
			}
		}
	});
	item_row(&f.item, warnings.as_ref())
}

fn needs_backup(plan: &DrainPlan) -> bool {
	plan.found.iter().any(|f| {
		f.verdict
			.as_ref()
			.is_some_and(|v| v.method == podkit_core::moves::Method::StopCopy)
	})
}

/// The dialog's body: the target, the destination for volume data, and each thing on the
/// server with what would become of it.
fn plan_markup(ctx: &TeamCtx, plan: &DrainPlan) -> Markup {
	let base = detail_url(ctx.team.id, plan.server.id);
	let refresh = format!("@get({})", js_string(&format!("{base}/drain/plan")));
	let moving = plan
		.found
		.iter()
		.filter(|f| f.item.outcome == Outcome::Pending)
		.count();
	html! {
		div id=(BODY) class="flex flex-col gap-4" {
			@if plan.targets.is_empty() {
				p class="text-sm text-muted-foreground" { "This team has only one server. Add another to drain this one into." }
			} @else {
				label class="flex flex-col gap-2 text-sm" {
					"Move the applications to"
					select class=(SELECT) "data-bind:drain_target" "data-on:change"=(refresh) {
						button { selectedcontent {} }
						option value="" { "Pick a server" }
						@for t in &plan.targets {
							option value=(t.id.0) disabled[t.status != ServerStatus::Active] {
								(t.name) @if t.status != ServerStatus::Active { " (not ready)" }
							}
						}
					}
				}
				@if needs_backup(plan) {
					label class="flex flex-col gap-2 text-sm" {
						"Copy volume data through"
						select class=(SELECT) "data-bind:drain_backup" "data-on:change"=(refresh) {
							button { selectedcontent {} }
							option value="" { "None: applications with volumes stay" }
							@for d in &plan.destinations {
								option value=(d.id.0) { (d.name) }
							}
						}
					}
				}
			}
			@if plan.found.is_empty() {
				p class="text-sm text-muted-foreground" { "Nothing is on this server." }
			} @else {
				p class="text-sm" {
					@if plan.target.is_some() {
						(moving) " of " (plan.found.len()) " things move, one at a time, quick ones first. The rest stay, for the reason given."
					} @else {
						"Pick a server to see what would move there."
					}
				}
				ul class="flex flex-col gap-2" aria-label="What is on this server" {
					@for f in &plan.found { (found_row(f)) }
				}
			}
		}
	}
}

/// The "Drain server" button, the dialog's opener.
#[must_use]
pub fn open_button(ctx: &TeamCtx, server: &Server) -> Markup {
	if !allowed(ctx) {
		return html! {};
	}
	let fetch = format!(
		"@get({})",
		js_string(&format!(
			"{}/drain/plan",
			detail_url(ctx.team.id, server.id)
		))
	);
	html! {
		span "data-on:click"=(fetch) {
			(dialog_open_button(DIALOG, BUTTON_OUTLINE, &html! { (icon("log-out")) "Drain server" }))
		}
	}
}

/// The dialog, with a placeholder body until the button asks for the real one.
#[must_use]
pub fn dialog_for(ctx: &TeamCtx, server: &Server) -> Markup {
	if !allowed(ctx) {
		return html! {};
	}
	let post = format!(
		"@post({})",
		js_string(&format!("{}/drain", detail_url(ctx.team.id, server.id)))
	);
	dialog(
		DIALOG,
		&format!("Drain {}", server.name),
		Some(
			"Moves every application that can move to another server, one at a time, and keeps anything new off this one until you stop draining it.",
		),
		&html! {
			div class="flex flex-col gap-3" data-signals="{drain_target: '', drain_backup: ''}" {
				div id=(BODY) class="flex flex-col gap-3" {
					p class="text-sm text-muted-foreground" { "Loading\u{2026}" }
				}
				(form_error())
			}
		},
		&html! {
			form method="dialog" { button class=(BUTTON_OUTLINE) autofocus { "Cancel" } }
			button type="button" class=(BUTTON) "data-on:click"=(post)
				"data-indicator:_busy" "data-attr:disabled"="$_busy || $drain_target === ''" {
				"Drain"
			}
		},
	)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Signals {
	drain_target: String,
	drain_backup: String,
}

impl Signals {
	fn target(&self) -> Option<ServerId> {
		self.drain_target.trim().parse().ok().map(ServerId)
	}

	fn backup(&self) -> Option<BackupTargetId> {
		self.drain_backup.trim().parse().ok().map(BackupTargetId)
	}
}

fn read(signals: Option<ReadSignals<Signals>>) -> Signals {
	signals.map(|ReadSignals(s)| s).unwrap_or_default()
}

async fn plan_body(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	signals: Option<ReadSignals<Signals>>,
) -> Response {
	let sig = read(signals);
	match service::drain_plan(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		id,
		sig.target(),
		sig.backup().is_some(),
	)
	.await
	{
		Ok(plan) => sse::patch(&plan_markup(&ctx, &plan)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// The section again, for its poll.
async fn section_fragment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Response {
	let result = async {
		let found = podkit_core::servers::service::get(&s.db, ctx.user.id, ctx.team.id, id).await?;
		let since = podkit_core::moves::draining_since(&s.db, id).await?;
		let latest = podkit_core::moves::latest_drain(&s.db, id).await?;
		Ok::<_, ServerError>(section(&ctx, &found.server, since, latest.as_ref()))
	}
	.await;
	match result {
		Ok(markup) => sse::patch(&markup).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn start(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	signals: Option<ReadSignals<Signals>>,
) -> Response {
	let sig = read(signals);
	let Some(target) = sig.target() else {
		return sse::error("Pick a server to move things to.").into_response();
	};
	match service::start_drain(&s, ctx.user.id, ctx.team.id, id, target, sig.backup()).await {
		Ok(_) => sse::redirect(&detail_url(ctx.team.id, id)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn stop(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::stop_draining(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(_) => sse::redirect(&detail_url(ctx.team.id, id)).into_response(),
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn signals_name_the_target_and_destination() {
		let s = Signals {
			drain_target: "7".to_owned(),
			drain_backup: String::new(),
		};
		assert_eq!(s.target(), Some(ServerId(7)));
		assert_eq!(s.backup(), None);
	}
}
