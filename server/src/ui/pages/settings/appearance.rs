//! Appearance: the color theme. The choice is a cookie written by `POST /theme` (see
//! `ui::theme`), so these buttons are a plain form and work without script.

use axum::Router;
use axum::routing::get;
use maud::{Markup, html};

use super::frame::settings_page;
use crate::state::AppState;
use crate::ui::components::{card, card_content, card_heading, icon};
use crate::ui::extract::TeamCtx;
use crate::ui::theme::{self, Theme};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/settings/appearance", get(page))
}

async fn page(ctx: TeamCtx) -> Markup {
	let current = theme::current();
	let option = |choice: Theme, label: &str, hint: &str, icon_name: &str| {
		let chosen = choice == current;
		html! {
			button type="submit" name="theme" value=(choice.as_str()) aria-pressed=(if chosen { "true" } else { "false" })
				class=(if chosen { SELECTED } else { UNSELECTED }) {
				span class="flex items-center gap-2 text-sm font-medium" {
					(icon(icon_name))
					(label)
					@if chosen { span class="ml-auto" { (icon("check")) } }
				}
				span class="text-left text-xs font-normal text-muted-foreground" { (hint) }
			}
		}
	};
	let content = html! {
		(card(&html! {
			(card_heading("Theme", Some("Dark is the default. System follows your device. The choice is kept in this browser.")))
			(card_content(&html! {
				form method="post" action="/theme" class="grid gap-3 sm:grid-cols-3" {
					(option(Theme::System, "System", "Match the device setting.", "monitor"))
					(option(Theme::Light, "Light", "Light surfaces.", "sun"))
					(option(Theme::Dark, "Dark", "Dark surfaces.", "moon"))
				}
			}))
		}))
		(card(&html! {
			(card_heading("Sidebar", Some("Fold it to icons for more room.")))
			(card_content(&html! {
				p class="text-sm text-muted-foreground" {
					"Use the panel button at the left of the header. podkit remembers it in this browser."
				}
			}))
		}))
	};
	settings_page(&ctx, "Appearance", &content)
}

const SELECTED: &str = "flex cursor-pointer flex-col gap-1 rounded-lg border border-foreground bg-accent p-4 text-accent-foreground outline-hidden focus-visible:ring-2 focus-visible:ring-ring/50";
const UNSELECTED: &str = "flex cursor-pointer flex-col gap-1 rounded-lg border border-border bg-card p-4 text-card-foreground outline-hidden transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring/50";
