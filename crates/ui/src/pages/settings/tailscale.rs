//! Settings → Tailscale: how the team's sidecars join a tailnet (an auth key, an optional
//! Headscale address, the tags they advertise), and how to put podkit itself on the tailnet.
//!
//! The auth key is write-only: the page says whether one is stored and never shows it, not
//! even masked. The form keeps it in a signal only until it is saved, and the answer is a
//! fresh page, so the typed key leaves the form too.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;

use super::frame::settings_page;
use crate::components::{
	INPUT, LABEL, Size, Tone, Variant, badge, button_class, card, card_content, card_heading,
	save_bar, save_bar_saved,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::sse::{self, js_string};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::state::AppState;
use podkit_core::tailscale::Settings;
use podkit_core::tailscale::service::{self, SettingsInput};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/settings/tailscale", get(page).post(save))
		.route(
			"/teams/{team_id}/settings/tailscale/key/remove",
			post(remove_key),
		)
}

fn url(ctx: &TeamCtx) -> String {
	format!("/teams/{}/settings/tailscale", ctx.team.id)
}

fn settings_form(ctx: &TeamCtx, settings: &Settings) -> Markup {
	let base = url(ctx);
	let initial = serde_json::json!({
		"ts_key": "",
		"ts_login_server": settings.login_server.clone().unwrap_or_default(),
		"ts_tags": settings.tags.join(" "),
		"_error": "",
	});
	let save = format!("evt.preventDefault(); @post({})", js_string(&base));
	let remove = format!("@post({})", js_string(&format!("{base}/key/remove")));
	html! {
		div class="flex flex-col gap-6" data-signals=(initial.to_string()) {
			form class="flex flex-col gap-6" "data-on:submit"=(save) {
				(card(&html! {
					(card_heading("Tailscale auth key", Some("Used to start the sidecar of each application you put on the tailnet. It is stored encrypted, sent only to the sidecar, and can't be read back.")))
					(card_content(&html! {
						div class="flex flex-col gap-4" {
							div class="flex items-center gap-3 text-sm" {
								@if settings.has_auth_key {
									(badge(Tone::Success, "Stored"))
								} @else {
									(badge(Tone::Neutral, "Not set"))
									span class="text-muted-foreground" { "Create one in the Tailscale admin console, under Settings → Keys." }
								}
							}
							div class="flex max-w-md flex-col gap-2" {
								label class=(LABEL) for="ts-key" {
									@if settings.has_auth_key { "Replace the auth key" } @else { "Auth key" }
								}
								input id="ts-key" class={ (INPUT) " font-mono" } type="password" name="ts_key" maxlength="512"
									autocomplete="new-password" autocapitalize="off" autocorrect="off" spellcheck="false"
									aria-describedby="ts-key-help" placeholder="tskey-auth-…" "data-bind:ts_key";
								p id="ts-key-help" class="text-sm text-muted-foreground" {
									"A reusable key works for every application. A key that expires is only needed the first time a node joins."
								}
							}
							@if settings.has_auth_key {
								div {
									button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(remove) { "Remove the key" }
								}
							}
						}
					}))
				}))
				(card(&html! {
					(card_heading("Connection", Some("Leave both empty to join Tailscale itself with the key's own tags.")))
					(card_content(&html! {
						div class="flex max-w-md flex-col gap-4" {
							div class="flex flex-col gap-2" {
								label class=(LABEL) for="ts-login-server" { "Login server (Headscale)" }
								input id="ts-login-server" class={ (INPUT) " font-mono" } type="url" name="ts_login_server" maxlength="255"
									autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false"
									aria-describedby="ts-login-help" placeholder="https://headscale.example.com" "data-bind:ts_login_server";
								p id="ts-login-help" class="text-sm text-muted-foreground" {
									"Only for a self-hosted Headscale control server. Use its address and a pre-auth key from it."
								}
							}
							div class="flex flex-col gap-2" {
								label class=(LABEL) for="ts-tags" { "Tags" }
								input id="ts-tags" class={ (INPUT) " font-mono" } type="text" name="ts_tags" maxlength="500"
									autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false"
									aria-describedby="ts-tags-help" placeholder="tag:podkit" "data-bind:ts_tags";
								p id="ts-tags-help" class="text-sm text-muted-foreground" {
									"Tags the sidecars advertise, separated by spaces. The auth key must be allowed to use them."
								}
							}
						}
					}))
				}))
				(save_bar(&["ts_key", "ts_login_server", "ts_tags"]))
			}
		}
	}
}

fn control_plane() -> Markup {
	html! {
		(card(&html! {
			(card_heading("podkit on the tailnet", Some("Keep podkit's own web UI off the public internet.")))
			(card_content(&html! {
				div class="flex flex-col gap-3 text-sm" {
					p { "On the machine that runs podkit, start " code class="font-mono" { "tailscale serve --bg 8080" } " and set " code class="font-mono" { "HOST=127.0.0.1" } " in podkit's configuration. Only your tailnet reaches it then, over HTTPS." }
					p { "Or set " code class="font-mono" { "HOST" } " to the machine's Tailscale address (the " code class="font-mono" { "100.x.y.z" } " one), and podkit listens on that interface alone." }
					p class="text-muted-foreground" { "The steps, with Headscale and the firewall, are in " code class="font-mono" { "docs/tailscale.md" } "." }
				}
			}))
		}))
	}
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let settings = service::settings(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_update = ctx.perms.can(Action::Update, Resource::Team);
	let content = html! {
		div class="flex flex-col gap-6" {
			@if can_update {
				(settings_form(&ctx, &settings))
			} @else {
				(card(&html! {
					(card_heading("Tailscale", None))
					(card_content(&html! {
						div class="flex flex-col gap-2 text-sm" {
							p { "Auth key: " @if settings.has_auth_key { "stored" } @else { "not set" } }
							@if let Some(server) = &settings.login_server { p { "Login server: " code class="font-mono" { (server) } } }
							@if !settings.tags.is_empty() { p { "Tags: " code class="font-mono" { (settings.tags.join(" ")) } } }
							p class="text-muted-foreground" { "Only people who can update the team may change these." }
						}
					}))
				}))
			}
			(control_plane())
		}
	};
	Ok(settings_page(&ctx, "Tailscale", &content))
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(clippy::struct_field_names)]
struct Signals {
	ts_key: String,
	ts_login_server: String,
	ts_tags: String,
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let input = SettingsInput {
		auth_key: &sig.ts_key,
		clear_auth_key: false,
		login_server: &sig.ts_login_server,
		tags: &sig.ts_tags,
	};
	match service::save_settings(&s, ctx.user.id, ctx.team.id, &input).await {
		// A fresh page, so the typed key leaves the form too.
		Ok(_) => sse::signals(&serde_json::json!({ "_error": "", "ts_key": "" }))
			.and(save_bar_saved())
			.and(sse::redirect(&url(&ctx)))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove_key(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	let result = async {
		let current = service::settings(&s, ctx.user.id, ctx.team.id).await?;
		let tags = current.tags.join(" ");
		service::save_settings(
			&s,
			ctx.user.id,
			ctx.team.id,
			&SettingsInput {
				auth_key: "",
				clear_auth_key: true,
				login_server: current.login_server.as_deref().unwrap_or_default(),
				tags: &tags,
			},
		)
		.await
	}
	.await;
	match result {
		Ok(_) => sse::redirect(&url(&ctx)).into_response(),
		Err(e) => error::action(&e),
	}
}
