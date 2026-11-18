//! The team's Hugging Face token: set, replace, remove, and whether one is
//! configured. The token is never shown, not even masked.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::{crumbs, models_url};
use crate::auth::permission::{Action, Resource};
use crate::model_files::token;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, INPUT, LABEL, Tone, card, card_content, card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/models/settings", get(page))
		.route("/teams/{team_id}/models/settings/token", post(set))
		.route(
			"/teams/{team_id}/models/settings/token/remove",
			post(remove),
		)
}

fn status_region(ctx: &TeamCtx, configured: bool) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::ModelFile);
	let base = format!("{}/settings/token", models_url(ctx.team.id));
	html! {
		div id="hf-token" class="flex flex-col gap-4" {
			div class="flex items-center gap-3 text-sm" {
				@if configured {
					span class=(Tone::Success.badge_class()) { "Configured" }
				} @else {
					span class=(Tone::Neutral.badge_class()) { "Not set" }
					span class="text-muted-foreground" { "Public models download without one." }
				}
			}
			@if can_update {
				form class="flex flex-col gap-3" "data-on:submit"={ "evt.preventDefault(); @post('" (base) "', {contentType: 'form'})" } {
					label class=(LABEL) {
						@if configured { "Replace the token" } @else { "Token" }
						input class=(INPUT) type="password" name="token" required maxlength="512" autocomplete="off" placeholder="hf_…";
					}
					(form_error())
					div class="flex gap-3" {
						button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
							@if configured { "Replace" } @else { "Save" }
						}
						@if configured {
							button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post('" (base) "/remove')" } { "Remove" }
						}
					}
				}
			}
		}
	}
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Response, Response> {
	let configured = token::is_configured(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let body = html! {
		div class="flex flex-col gap-6" data-signals="{_error: ''}" {
			h1 class="text-xl font-semibold" { "Model settings" }
			(card(&html! {
				(card_header("Hugging Face token", Some("Needed for gated models and to avoid rate limits. It is stored encrypted, sent only to the downloader, and can't be read back.")))
				(card_content(&status_region(&ctx, configured)))
			}))
		}
	};
	Ok(app_page(
		&ctx,
		"Model settings",
		&crumbs(&ctx, vec![("Settings", None)]),
		&body,
	)
	.into_response())
}

#[derive(Deserialize)]
struct TokenSignals {
	token: String,
}

async fn set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	axum::Form(sig): axum::Form<TokenSignals>,
) -> Response {
	match token::store(&s, ctx.user.id, ctx.team.id, &sig.token).await {
		// A fresh page, so the typed token leaves the form too.
		Ok(()) => sse::redirect(&format!("{}/settings", models_url(ctx.team.id))).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	match token::clear(&s, ctx.user.id, ctx.team.id).await {
		Ok(()) => sse::patch(&status_region(&ctx, false))
			.and(sse::toast("Token removed"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}
