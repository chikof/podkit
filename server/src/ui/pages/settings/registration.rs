//! Registration: who may create an account on this instance. It is a setting of the whole
//! podkit instance (the `REGISTRATION` variable or `registration` in `config.toml`), not of
//! a team, so the page shows it and says how to change it; it has no controls.

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};

use super::frame::settings_page;
use crate::auth::service::registration_allowed;
use crate::state::{AppState, Registration};
use crate::ui::components::{Tone, badge, card, card_content, card_heading};
use crate::ui::error;
use crate::ui::extract::TeamCtx;

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/settings/registration", get(page))
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
	let (name, tone, value, text) = describe(policy);
	let content = html! {
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
	};
	Ok(settings_page(&ctx, "Registration", &content))
}
