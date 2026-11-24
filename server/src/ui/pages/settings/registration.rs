//! Registration: who may create an account on this instance. It is a setting of the whole
//! podkit instance (the `REGISTRATION` variable or `registration` in `config.toml`), not of
//! a team, so the page shows it and says how to change it; it has no controls.
//!
//! Below it, the other instance-wide policy a small team asks about: whether podkit may send
//! requests to private network addresses (a Gotify on the LAN, a Coolify at `192.168.x.x`).
//! The instance administrator switches it here; `OUTBOUND_PRIVATE` pins it.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use tower_cookies::Cookies;

use super::frame::settings_page;
use crate::auth::service::registration_allowed;
use crate::net::outbound::{self as net, Source};
use crate::quotas::service::is_instance_admin;
use crate::state::{AppState, Registration};
use crate::ui::components::{
	Tone, badge, card, card_content, card_heading, save_bar, save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::{self, js_string};
use crate::ui::{error, shell};

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/settings/registration",
		get(page).post(save_outbound),
	)
}

fn url(team: crate::ids::TeamId) -> String {
	format!("/teams/{team}/settings/registration")
}

const fn describe(policy: Registration) -> (&'static str, Tone, &'static str, &'static str) {
	match policy {
		Registration::FirstUser => (
			"First user only",
			Tone::Neutral,
			"first-user",
			"Only while no account exists. The first person to sign up becomes the administrator; after that the sign-up page is closed and people are added to a team by someone already in it.",
		),
		Registration::Open => (
			"Open",
			Tone::Warning,
			"open",
			"Anyone who can reach this instance may create an account. Use it only when that is what you want, for example on a hosted instance behind your own sign-in.",
		),
		Registration::Closed => (
			"Closed",
			Tone::Neutral,
			"closed",
			"Nobody can create an account. Add people to a team from the Members page; they need an account first, so switch to open for a moment to let them sign up.",
		),
	}
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let policy = s.config.registration;
	let open_now = registration_allowed(&s.db, policy)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let outbound = net::status(&s)
		.await
		.map_err(|e| error::page(&ctx, &e.into()))?;
	let admin = is_instance_admin(&s.db, ctx.user.id)
		.await
		.map_err(|e| error::page(&ctx, &e.into()))?;
	let (name, tone, value, text) = describe(policy);
	let content = html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_heading("Sign-ups on this instance", Some("Who may create an account.")))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						div class="flex items-center gap-3" {
							(badge(tone, name))
							span class="text-sm text-muted-foreground" {
								@if open_now { "The sign-up page is open right now." } @else { "The sign-up page is closed right now." }
							}
						}
						p class="text-sm" { (text) }
						p class="text-sm text-muted-foreground" {
							"This is set for the whole instance, not for this team. Change it with "
							code class="rounded-sm bg-muted px-1 py-0.5 font-mono text-xs" { "REGISTRATION=" (value) }
							" (or " code class="rounded-sm bg-muted px-1 py-0.5 font-mono text-xs" { "registration" }
							" in " code class="rounded-sm bg-muted px-1 py-0.5 font-mono text-xs" { "config.toml" }
							") and restart podkit."
						}
					}
				}))
			}))
			(outbound_card(&ctx, outbound, admin))
		}
	};
	Ok(settings_page(&ctx, "Registration", &content))
}

fn outbound_card(ctx: &TeamCtx, status: net::Status, admin: bool) -> Markup {
	let on = status.allow_private;
	let pinned = status.source == Source::Env;
	let code = "rounded-sm bg-muted px-1 py-0.5 font-mono text-xs";
	card(&html! {
		(card_heading("Requests to private networks", Some("Whether podkit may reach addresses on your own network when you point it at one.")))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				div class="flex items-center gap-3" {
					(badge(if on { Tone::Warning } else { Tone::Neutral }, if on { "Allowed" } else { "Blocked" }))
					span class="text-sm text-muted-foreground" {
						@match status.source {
							Source::Env => { "Set by " code class=(code) { "OUTBOUND_PRIVATE" } "." }
							Source::Setting => { "Set by the instance administrator." }
							Source::Default => { "The default for this instance's sign-up policy." }
						}
					}
				}
				p class="text-sm" {
					"Notification channels, imports, uptime checks, git providers, identity providers, backup storage and log export send requests to addresses people type. "
					"Loopback (127.0.0.1, ::1), link-local and cloud metadata (169.254.169.254) addresses and podkit's own address are never reachable, and a redirect is never followed. "
					"Private networks (10.x, 172.16–31.x, 192.168.x, 100.64–127.x and fc00::/7) are reachable only while this is on."
				}
				@if on {
					p class="text-sm" {
						"With this on, anyone in any team who may create a notification channel, import or git provider can make podkit's host send requests to your private network. "
						"That is what a single-owner homelab wants (an ntfy or Gotify on the LAN) and not what a shared instance should allow. Turn it off if people you do not fully trust have an account."
					}
				}
				@if admin && !pinned {
					form "data-on:submit"={ "evt.preventDefault(); @post(" (js_string(&url(ctx.team.id))) ")" }
						"data-signals"=(json!({ "outbound": on, "_error": "" }).to_string()) {
						div class="ui-check-row" {
							input id="outbound-private" type="checkbox" switch role="switch" class="ui-switch" "data-bind:outbound" aria-describedby="outbound-private-help";
							div {
								label for="outbound-private" class="ui-check-title" { "Allow requests to private network addresses" }
								small id="outbound-private-help" { "Applies at once to every team on this instance." }
							}
						}
						(save_bar(&["outbound"]))
					}
				} @else if pinned {
					p class="text-sm text-muted-foreground" {
						"Change it with " code class=(code) { "OUTBOUND_PRIVATE=allow" } " or " code class=(code) { "OUTBOUND_PRIVATE=deny" }
						" and restart podkit."
					}
				} @else {
					p class="text-sm text-muted-foreground" { "Only the administrator of this podkit instance can change it." }
				}
			}
		}))
	})
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OutboundSignals {
	outbound: bool,
}

async fn save_outbound(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<OutboundSignals>,
) -> Response {
	match net::set_private(&s, ctx.user.id, ctx.team.id, sig.outbound).await {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Saved.");
			sse::signals(&json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&url(ctx.team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}
