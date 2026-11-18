//! The Advanced tab: remove the stack. The containers and the stack's network go; its
//! volumes (the data) stay unless the box is ticked. The name has to be typed.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::frame::{self, Tab};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ProjectId, StackId, TeamId};
use crate::stacks::service;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, CHECKBOX, INPUT, LABEL, card, card_content, card_header,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::sse::{self, js_string};

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	if !ctx.perms.can(Action::Delete, Resource::Stack) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let l = frame::load(&s, &ctx, project, id).await?;
	let stack = l.stack();
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/remove", l.base(ctx.team.id)))
	);
	let signals = json!({ "confirm": "", "purge": false, "_error": "" });
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(signals.to_string()) {
			(card(&html! {
				(card_header("Remove this stack", Some("Stops and removes every container and the stack's private network.")))
				(card_content(&html! {
					form class="flex max-w-xl flex-col gap-4" "data-on:submit"=(post) {
						label class="flex cursor-pointer items-start gap-3 text-sm" {
							input type="checkbox" class=(CHECKBOX) "data-bind:purge";
							span class="flex flex-col gap-1" {
								span class="font-medium" { "Also delete its volumes" }
								span class="text-muted-foreground" { "The data the services keep (databases, uploads) is deleted for good. Without this the volumes stay on the server." }
							}
						}
						label class=(LABEL) {
							span { "Type " strong class="font-mono" { (stack.name) } " to confirm" }
							input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:confirm";
						}
						(form_error())
						div {
							button type="submit" class=(BUTTON_DANGER) "data-indicator:_busy"
								"data-attr:disabled"=(format!("$_busy || $confirm !== {}", js_string(&stack.name))) { "Remove stack" }
						}
					}
				}))
			}))
		}
	};
	Ok(frame::page(&ctx, &l, Tab::Advanced, &content))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Signals {
	confirm: String,
	purge: bool,
}

pub(super) async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		// Typing the name is part of the action, so a forged request has to do it too.
		let stack = service::find(&s.db, ctx.user.id, ctx.team.id, project, id).await?;
		if sig.confirm != stack.name {
			return Err(ServerError::Validation(
				"Type the stack's name exactly to confirm.".to_owned(),
			));
		}
		service::remove(&s, ctx.user.id, ctx.team.id, project, id, sig.purge).await?;
		Ok(stack)
	}
	.await;
	match result {
		Ok(stack) => sse::redirect(&format!(
			"{}?environment={}",
			super::project_url(ctx.team.id, project),
			stack.environment_id.0
		))
		.into_response(),
		Err(e) => error::action(&e),
	}
}
