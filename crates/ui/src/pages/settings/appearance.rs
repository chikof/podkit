//! Appearance: the color theme. The choice is a cookie written by `POST /theme` (see
//! `ui::theme`), so these buttons are a plain form and work without script.

use axum::Router;
use axum::routing::get;
use maud::{Markup, html};

use super::frame::settings_page;
use crate::components::{card, card_content, card_heading, icon};
use crate::extract::TeamCtx;
use crate::theme::{self, Theme};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/settings/appearance", get(page))
}

async fn page(ctx: TeamCtx) -> Markup {
	let current = theme::current();
	let option = |choice: Theme, label: &str, hint: &str, icon_name: &str| {
		html! {
			button type="submit" name="theme" value=(choice.as_str()) aria-pressed=(if choice == current { "true" } else { "false" })
				class="ui-theme-opt" {
				span class="ui-theme-opt-head" {
					(icon(icon_name))
					(label)
					@if choice == current { span class="ui-theme-opt-check" { (icon("check")) } }
				}
				span class="ui-theme-opt-hint" { (hint) }
			}
		}
	};
	let content = html! {
		(card(&html! {
			(card_heading("Theme", Some("Follows your system by default. The choice is kept in this browser.")))
			(card_content(&html! {
				form method="post" action="/theme" class="ui-theme-opts" {
					(option(Theme::System, "System", "Match the device setting.", "monitor"))
					(option(Theme::Light, "Light", "Light surfaces.", "sun"))
					(option(Theme::Dark, "Dark", "Dark surfaces.", "moon"))
				}
			}))
		}))
	};
	settings_page(&ctx, "Appearance", &content)
}
