//! A server's image scanning settings, a section of the server's page: scanning on or off
//! (on by default), and the CPU and memory its scanner containers may use.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ServerId, TeamId};
use crate::scanning::service::{self, Settings};
use crate::state::AppState;
use crate::ui::components::{BUTTON, INPUT, LABEL, SWITCH};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

/// The signals the section starts with: what is saved.
fn signals(settings: Settings) -> String {
	json!({
		"scan_enabled": settings.enabled,
		"scan_cpus": settings.cpus,
		"scan_memory": settings.memory_mb,
		"_scan_error": "",
		"_scan_busy": false,
	})
	.to_string()
}

/// The section for the server's page. Without `update scan` it shows the values and cannot
/// change them; a forged post is refused by the service anyway.
pub fn settings_card(ctx: &TeamCtx, server: ServerId, settings: Settings) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Scan);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("/teams/{}/security/servers/{server}", ctx.team.id))
	);
	html! {
		section class="ui-stack" aria-labelledby="scan-title" data-signals=(signals(settings)) {
			h2 id="scan-title" class="ui-sec-h" { "Image scanning" }
			p class="ui-lede" {
				"Every image this server runs is scanned for known vulnerabilities when it deploys and once a day. "
				"The scanner runs in a container with the limits below. A finding warns; it never blocks a deploy."
			}
			form class="flex max-w-md flex-col gap-4" "data-on:submit"=(post) {
				fieldset class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" disabled[!can_update] {
					legend class="ui-visually-hidden" { "Image scanning" }
					div class="ui-check-row" {
						input id="scan-enabled" type="checkbox" switch role="switch" class=(SWITCH) "data-bind:scan_enabled"
							aria-describedby="scan-enabled-help";
						div {
							label for="scan-enabled" class="ui-check-title" { "Scan images" }
							small id="scan-enabled-help" { "Turn it off to stop scanning on this server. Earlier results stay." }
						}
					}
					div class="grid gap-4 sm:grid-cols-2" {
						label class=(LABEL) {
							"CPUs"
							input class=(INPUT) type="number" min="0.25" max="8" step="0.25" required inputmode="decimal" "data-bind:scan_cpus";
						}
						label class=(LABEL) {
							"Memory (MiB)"
							input class=(INPUT) type="number" min="256" max="16384" step="64" required inputmode="numeric" "data-bind:scan_memory";
						}
					}
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_scan_error" "data-text"="$_scan_error" style="display: none" {}
				@if can_update {
					div {
						button type="submit" class=(BUTTON) "data-indicator:_scan_busy" "data-attr:disabled"="$_scan_busy" { "Save" }
					}
				} @else {
					p class="text-sm text-muted-foreground" { "Only people who can update scanning may change these." }
				}
			}
		}
	}
}

/// A number or a numeric string: a number input's signal is one or the other.
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
	match Value::deserialize(d)? {
		Value::Number(n) => n.as_f64().ok_or_else(|| serde::de::Error::custom("number")),
		Value::String(s) => s
			.trim()
			.parse()
			.map_err(|_| serde::de::Error::custom("not a number")),
		_ => Err(serde::de::Error::custom("not a number")),
	}
}

#[derive(Deserialize)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the form's signals, which share the `scan_` prefix"
)]
pub(super) struct Signals {
	scan_enabled: bool,
	#[serde(deserialize_with = "number")]
	scan_cpus: f64,
	#[serde(deserialize_with = "number")]
	scan_memory: f64,
}

fn refuse(message: &str) -> Response {
	sse::signals(&json!({ "_scan_error": message })).into_response()
}

pub(super) async fn update(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	if !(256.0..=16_384.0).contains(&sig.scan_memory) || sig.scan_memory.fract() != 0.0 {
		return refuse("Memory must be a whole number of MiB from 256 to 16384.");
	}
	#[allow(
		clippy::cast_possible_truncation,
		reason = "checked to be a whole number from 256 to 16384 just above"
	)]
	let memory_mb = sig.scan_memory as i32;
	let new = Settings {
		enabled: sig.scan_enabled,
		cpus: sig.scan_cpus,
		memory_mb,
	};
	match service::update_settings(&s.db, ctx.user.id, ctx.team.id, server, new).await {
		Ok(()) => sse::signals(&json!({ "_scan_error": "" }))
			.and(sse::toast("Scanning settings saved."))
			.into_response(),
		Err(ServerError::ServerNotFound) => error::page(&ctx, &ServerError::ServerNotFound),
		Err(e) => refuse(&error::message(&e)),
	}
}
