//! The "Private access (Tailscale)" card of the Domains tab: serve the application on the
//! team's tailnet, at `https://<name>.<tailnet>.ts.net`, by a sidecar next to it.
//!
//! The card is one fragment (`#tailnet-card`). Starting and removing run in a job, so while
//! the exposure is in flight the card polls itself every 3 seconds and stops when it settles.
//! The page never shows the auth key: it only says whether one is stored, and links to
//! Settings → Tailscale where it is set.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;

use super::{AppPath, app_url};
use crate::components::{
	INPUT, LABEL, Size, Tone, Variant, badge, button_class, card, card_content, card_section, icon,
};
use crate::extract::TeamCtx;
use crate::sse::js_string;
use crate::{error, sse, status};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::state::AppState;
use podkit_core::tailscale::service::{self, AppView};
use podkit_core::tailscale::{Exposure, ExposureStatus};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/tailnet",
			get(poll).post(enable),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/tailnet/remove",
			post(disable),
		)
}

/// The signals the card binds: the node name field and its own error line.
pub fn signals(view: &AppView) -> serde_json::Value {
	serde_json::json!({
		"tn_hostname": view
			.exposure
			.as_ref()
			.map_or(view.suggested_hostname.as_str(), |e| e.hostname.as_str()),
		"_tn_error": "",
	})
}

fn status_badge(status: ExposureStatus) -> Markup {
	match status {
		ExposureStatus::Starting => badge(Tone::Progress, "Starting"),
		ExposureStatus::Running => badge(Tone::Success, "On the tailnet"),
		ExposureStatus::Failed => badge(Tone::Danger, "Failed"),
		ExposureStatus::Removing => badge(Tone::Progress, "Removing"),
	}
}

fn in_flight(exposure: &Exposure) -> bool {
	matches!(
		exposure.status,
		ExposureStatus::Starting | ExposureStatus::Removing
	)
}

fn error_line() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_tn_error" "data-text"="$_tn_error" style="display: none" {}
	}
}

/// The card. `base` is the application's address; `settings_url` is where the auth key is set.
pub fn tailnet_card(
	base: &str,
	view: &AppView,
	can_create: bool,
	can_delete: bool,
	settings_url: &str,
) -> Markup {
	let poll_url = format!("{base}/tailnet");
	let enable = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/tailnet"))
	);
	let remove = format!("@post({})", js_string(&format!("{base}/tailnet/remove")));
	html! {
		div id="tailnet-card" {
			(card(&html! {
				(card_section(
					"Private access (Tailscale)",
					Some("Serve this application only on your tailnet, with no public route. It gets an https address that only your tailnet devices can open."),
					None,
				))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						@if let Some(exposure) = &view.exposure {
							(status::poll(&poll_url, in_flight(exposure)))
							div class="flex min-w-0 flex-wrap items-center gap-3" {
								(status_badge(exposure.status))
								span class="text-sm text-muted-foreground" { "Node " code class="font-mono" { (exposure.hostname) } }
							}
							@if let Some(url) = exposure.url() {
								a href=(url) class="inline-flex min-w-0 items-center gap-1 text-sm font-medium hover:underline" target="_blank" rel="noopener noreferrer" {
									span class="break-all" { (url) }
									(icon("external-link"))
								}
								p class="text-sm text-muted-foreground" { "Open it from a device signed in to the same tailnet." }
							}
							@if let Some(message) = &exposure.error {
								p class="text-sm text-destructive" role="alert" { (message) }
							}
							@if exposure.status == ExposureStatus::Starting {
								p class="text-sm text-muted-foreground" { "The sidecar is starting and joining the tailnet. This can take a minute while its image is pulled." }
							}
							(error_line())
							div class="flex flex-wrap gap-3" {
								@if exposure.status == ExposureStatus::Failed && can_create {
									button type="button" class=(button_class(Variant::Default, Size::Md))
										"data-on:click"={ "$tn_hostname = " (js_string(&exposure.hostname)) "; @post(" (js_string(&format!("{base}/tailnet"))) ")" } { "Try again" }
								}
								@if can_delete && exposure.status != ExposureStatus::Removing {
									button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(remove) { "Turn off" }
								}
							}
							p class="text-sm text-muted-foreground" { "Turning it off signs the node out of the tailnet and removes the sidecar and its saved identity." }
						} @else if !view.settings.has_auth_key {
							p class="text-sm" {
								"No auth key is stored yet. Add one in "
								a href=(settings_url) class="font-medium underline" { "Settings → Tailscale" }
								", then come back here."
							}
						} @else if can_create {
							form class="flex flex-col gap-4" "data-on:submit"=(enable) {
								label class=(LABEL) for="tn-hostname" {
									"Node name"
								}
								input id="tn-hostname" class={ (INPUT) " font-mono" } type="text" required maxlength="63" autocomplete="off" spellcheck="false" autocapitalize="off" autocorrect="off"
									aria-describedby="tn-hostname-help" "data-bind:tn_hostname";
								p id="tn-hostname-help" class="text-sm text-muted-foreground" {
									"It becomes the first part of the address: " code class="font-mono" { "<name>.<your tailnet>.ts.net" } ". Turning it on restarts the running application once, so the tailnet can reach it."
								}
								(error_line())
								div {
									button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Turn on" }
								}
							}
						} @else {
							p class="text-sm text-muted-foreground" { "This application is not on the tailnet." }
						}
					}
				}))
			}))
		}
	}
}

async fn refreshed(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Result<Markup, podkit_core::error::ServerError> {
	let view = service::view(s, ctx.user.id, ctx.team.id, project, app).await?;
	let base = app_url(ctx.team.id, project, app);
	Ok(tailnet_card(
		&base,
		&view,
		ctx.perms.can(Action::Create, Resource::Domain),
		ctx.perms.can(Action::Delete, Resource::Domain),
		&settings_url(ctx.team.id),
	))
}

pub fn settings_url(team: TeamId) -> String {
	format!("/teams/{team}/settings/tailscale")
}

/// The card again: what the 3-second poll fetches. A team that does not own the application
/// gets the 404 that ends the poll.
async fn poll(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
) -> Response {
	match refreshed(&s, &ctx, project, app).await {
		Ok(card) => sse::patch(&card).into_response(),
		Err(e) => error::bare(Some(&ctx.user), &e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Signals {
	tn_hostname: String,
}

fn failure(e: &podkit_core::error::ServerError) -> Response {
	sse::signals(&serde_json::json!({ "_tn_error": error::message(e) })).into_response()
}

async fn enable(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		service::enable(&s, ctx.user.id, ctx.team.id, project, app, &sig.tn_hostname).await?;
		refreshed(&s, &ctx, project, app).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card)
			.and(sse::signals(&serde_json::json!({ "_tn_error": "" })))
			.into_response(),
		Err(e) => failure(&e),
	}
}

async fn disable(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): AppPath,
) -> Response {
	let result = async {
		service::disable(&s, ctx.user.id, ctx.team.id, project, app).await?;
		refreshed(&s, &ctx, project, app).await
	}
	.await;
	match result {
		Ok(card) => sse::patch(&card)
			.and(sse::signals(&serde_json::json!({ "_tn_error": "" })))
			.into_response(),
		Err(e) => failure(&e),
	}
}
