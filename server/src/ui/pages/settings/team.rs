//! Team settings: the team's name. Renaming needs `update team`; without it the page is
//! read-only and a forged request is refused by the service.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use tower_cookies::Cookies;

use super::frame::settings_page;
use crate::auth::permission::{Action, Resource};
use crate::state::AppState;
use crate::teams::service;
use crate::ui::components::{
	INPUT, LABEL, Tone, card, card_content, card_heading, save_bar, save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;
use crate::ui::{error, shell, sse};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/settings", get(page).post(rename))
}

async fn page(ctx: TeamCtx) -> Markup {
	let can_rename = ctx.perms.can(Action::Update, Resource::Team);
	let signals = format!("{{name: {}, _error: ''}}", js_string(&ctx.team.name));
	let url = format!("/teams/{}/settings", ctx.team.id);
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(signals) {
			@if can_rename {
				form class="flex flex-col gap-6" "data-on:submit"={ "evt.preventDefault(); @post(" (js_string(&url)) ")" } {
					(card(&html! {
						(card_heading("Team name", Some("Shown in the sidebar and the team switcher.")))
						(card_content(&html! {
							label class=(LABEL) {
								"Name"
								input class={ (INPUT) " max-w-md" } type="text" required maxlength="60" autocomplete="off" "data-bind:name";
							}
						}))
					}))
					(save_bar(&["name"]))
				}
			} @else {
				(card(&html! {
					(card_heading("Team name", Some("Shown in the sidebar and the team switcher.")))
					(card_content(&html! {
						p class="text-sm" { (ctx.team.name) }
						p class="mt-2 text-sm text-muted-foreground" { "Only people who can update the team may rename it." }
					}))
				}))
			}
			(card(&html! {
				(card_heading("Team slug", Some("The short name the team was created with. It does not change.")))
				(card_content(&html! {
					p class="font-mono text-sm" { (ctx.team.slug) }
				}))
			}))
		}
	};
	settings_page(&ctx, "Team", &content)
}

#[derive(Deserialize)]
struct RenameSignals {
	name: String,
}

async fn rename(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<RenameSignals>,
) -> Response {
	match service::rename(&s.db, ctx.user.id, ctx.team.id, &sig.name).await {
		Ok(team) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("Team renamed to {}.", team.name),
			);
			sse::signals(&serde_json::json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&format!("/teams/{}/settings", team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}
