//! Logs: the team's stored container logs, searched across deploys, and how long they are kept
//! and where they are forwarded.
//!
//! One page per team. The search ([`history`]) is a GET form, so a search is a link. The
//! retention and forwarding form ends in the save bar, like every settings form. The page
//! calls `log_search::service` for everything: it asks the agents, scopes to the team and
//! authorizes. The header of the forwarding endpoint is write-only: the page says whether one
//! is saved and never shows it.

pub mod history;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use self::history::{Panel, Params};
use crate::auth::permission::{Action, Resource};
use crate::ids::TeamId;
use crate::log_search::service::{self, SettingsInput};
use crate::log_search::{MAX_RETENTION_DAYS, MIN_MAX_MB, Settings};
use crate::state::AppState;
use crate::ui::components::{
	CHECKBOX, INPUT, LABEL, Tone, card, card_content, card_section, empty_state, save_bar,
	save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::app_page;
use crate::ui::sse::{self, js_string};

const HELP: &str = "text-xs font-normal text-muted-foreground";
use crate::ui::{error, shell};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/logs", get(page).post(save))
		.route("/teams/{team_id}/logs/stream", get(history::stream))
}

pub fn url(team: TeamId) -> String {
	format!("/teams/{team}/logs")
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(params): Query<Params>,
) -> Result<Markup, Response> {
	let sources = service::sources(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let settings = service::settings(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_edit = ctx.perms.can(Action::Update, Resource::Logs);
	let panel = Panel {
		team: ctx.team.id,
		action: url(ctx.team.id),
		keep: Vec::new(),
		fixed: None,
		sources: &sources,
	};
	let body = html! {
		div class="flex flex-col gap-6" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Logs" }
					p class="ui-lede" {
						"What your containers printed, kept on each server after the container is gone and searchable across deploys."
					}
				}
			}
			@if sources.is_empty() {
				(empty_state(
					"scroll-text",
					"Nothing to search yet.",
					Some("Logs are stored for the applications, databases, models and stacks of this team once they run."),
					None,
				))
			} @else {
				(card(&html! {
					(card_section("Search", Some("The newest matching lines of the range, oldest first. Times are UTC."), None))
					(card_content(&history::panel(&panel, &params)))
				}))
			}
			(settings_card(ctx.team.id, &settings, can_edit))
		}
	};
	Ok(app_page(&ctx, "Logs", &[("Logs", None)], &body))
}

const FIELDS: [&str; 7] = [
	"retention_days",
	"max_mb",
	"export_kind",
	"export_url",
	"export_tenant",
	"export_authorization",
	"clear_authorization",
];

fn kind_choice(value: &str, label: &str) -> Markup {
	html! {
		label class="flex items-center gap-2 text-sm font-normal" {
			input type="radio" name="export_kind" value=(value) "data-bind:export_kind";
			(label)
		}
	}
}

fn settings_card(team: TeamId, settings: &Settings, can_edit: bool) -> Markup {
	let export = settings.export.as_ref();
	if !can_edit {
		return card(&html! {
			(card_section("Retention and forwarding", None, None))
			(card_content(&html! {
				p class="text-sm" {
					"Lines are kept for " (settings.retention_days) " days or " (settings.max_mb) " MB per server, whichever comes first."
				}
				@if let Some(export) = export {
					p class="mt-2 text-sm" { "Forwarded to " (export.kind.label()) "." }
				}
				p class="mt-2 text-sm text-muted-foreground" { "Only people who can update logs may change this." }
			}))
		});
	}
	// Never the header itself: whether one is saved, and nothing else.
	let signals = json!({
		"retention_days": settings.retention_days,
		"max_mb": settings.max_mb,
		"export_kind": export.map_or("", |e| e.kind.as_str()),
		"export_url": export.map_or("", |e| e.url.as_str()),
		"export_tenant": export.and_then(|e| e.tenant.as_deref()).unwrap_or(""),
		"export_authorization": "",
		"clear_authorization": false,
		"_error": "",
	});
	let has_header = export.is_some_and(|e| e.has_authorization);
	let post = format!("evt.preventDefault(); @post({})", js_string(&url(team)));
	card(&html! {
		(card_section("Retention and forwarding", Some("Applies to every server of the team. Whichever limit is reached first deletes the oldest lines."), None))
		(card_content(&html! {
			form class="flex flex-col gap-4" data-signals=(signals.to_string()) "data-on:submit"=(post) {
				div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
					label class=(LABEL) {
						"Keep lines for (days)"
						input class=(INPUT) type="number" inputmode="numeric" required min="1" max=(MAX_RETENTION_DAYS)
							"data-bind:retention_days";
					}
					label class=(LABEL) {
						"Size cap per server (MB)"
						input class=(INPUT) type="number" inputmode="numeric" required min=(MIN_MAX_MB) "data-bind:max_mb";
						span class=(HELP) { "1024 MB is 1 GB." }
					}
				}
				fieldset class="m-0 flex min-w-0 flex-col gap-2 border-0 p-0" {
					legend class="mb-2 text-sm font-medium" { "Forward every line to" }
					div class="flex flex-wrap gap-x-6 gap-y-2" {
						(kind_choice("", "Nowhere"))
						(kind_choice("loki", "Loki"))
						(kind_choice("otlp", "OpenTelemetry (OTLP/HTTP)"))
					}
					span class=(HELP) {
						"Best effort: when the backend is slow or down, lines are dropped from the queue, never held back. They stay stored and searchable here."
					}
				}
				div class="flex flex-col gap-4" "data-show"="$export_kind !== ''" {
					label class=(LABEL) {
						"Address"
						input class={ (INPUT) " font-mono" } type="url" maxlength="500" autocomplete="off" autocapitalize="none"
							autocorrect="off" spellcheck="false" placeholder="https://loki.example.com/loki/api/v1/push"
							"data-bind:export_url";
						span class=(HELP) {
							"Loki: the push endpoint, /loki/api/v1/push. OTLP: the collector, /v1/logs."
						}
					}
					label class=(LABEL) "data-show"="$export_kind === 'loki'" {
						"Tenant (X-Scope-OrgID)"
						input class=(INPUT) type="text" maxlength="200" autocomplete="off" autocapitalize="none"
							autocorrect="off" spellcheck="false" "data-bind:export_tenant";
						span class=(HELP) { "Leave empty for a Loki without multi-tenancy." }
					}
					label class=(LABEL) {
						"Authorization header"
						input class={ (INPUT) " font-mono" } type="password" maxlength="2000" autocomplete="off"
							autocapitalize="none" autocorrect="off" spellcheck="false"
							placeholder=(if has_header { "Saved. Type a new value to replace it." } else { "Bearer …  or  Basic …" })
							"data-bind:export_authorization";
						span class=(HELP) { "Sent as the Authorization header. Write-only: it is never shown again." }
					}
					@if has_header {
						div class="ui-check" {
							input id="logs-clear-auth" type="checkbox" class=(CHECKBOX) "data-bind:clear_authorization";
							label for="logs-clear-auth" class="cursor-pointer" { "Forget the saved header" }
						}
					}
				}
				(save_bar(&FIELDS))
			}
		}))
	})
}

/// What the form sends.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Signals {
	retention_days: i32,
	max_mb: i32,
	export_kind: String,
	export_url: String,
	export_tenant: String,
	export_authorization: String,
	clear_authorization: bool,
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: tower_cookies::Cookies,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let input = SettingsInput {
		retention_days: sig.retention_days,
		max_mb: sig.max_mb,
		export_kind: sig.export_kind,
		export_url: sig.export_url,
		export_tenant: sig.export_tenant,
		export_authorization: sig.export_authorization,
		clear_authorization: sig.clear_authorization,
	};
	match service::update_settings(&s, ctx.user.id, ctx.team.id, &input).await {
		Ok(_) => {
			shell::flash(&cookies, Tone::Success, "Log settings saved.");
			sse::signals(
				&json!({ "_error": "", "export_authorization": "", "clear_authorization": false }),
			)
			.and(save_bar_saved())
			.and(sse::redirect(&url(ctx.team.id)))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}
