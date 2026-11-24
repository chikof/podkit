//! The Environment tab: the stack's variables, write-only. The page lists their names; a
//! value goes in through the form and never comes back out. A variable the compose file
//! uses and the stack lacks is called out with a button that fills in its name.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::frame::{self, Tab};
use crate::components::{
	BUTTON, INPUT, LABEL, Size, Variant, button_class, card, card_content, card_heading, icon,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::sse::{self, js_string};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::stacks::service::{self, UpdateInput};
use podkit_core::state::AppState;

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	let can_update = ctx.perms.can(Action::Update, Resource::Stack);
	let base = l.base(ctx.team.id);
	let set = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/env"))
	);
	let unset_url = js_string(&format!("{base}/env/unset"));
	let signals = json!({ "envKey": "", "envValue": "", "unsetKey": "", "_error": "" });
	let mut keys = l.detail.env_keys.clone();
	keys.sort();
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(signals.to_string()) {
			@if !l.detail.missing_vars.is_empty() && can_update {
				(card(&html! {
					(card_heading("Missing variables", Some("The compose file uses these and the stack does not set them.")))
					(card_content(&html! {
						ul class="flex flex-wrap gap-2" {
							@for name in &l.detail.missing_vars {
								li {
									button type="button" class=(button_class(Variant::Outline, Size::Sm))
										"data-on:click"=(format!("$envKey = {}; document.getElementById('env-value').focus()", js_string(name))) {
										span class="font-mono" { (name) } (icon("plus"))
									}
								}
							}
						}
					}))
				}))
			}
			(card(&html! {
				(card_heading("Variables", Some("Values are encrypted and never shown again. Set a name again to replace its value. Changes apply on the next deploy.")))
				(card_content(&html! {
					@if keys.is_empty() {
						p class="text-sm text-muted-foreground" { "No variables set." }
					} @else {
						ul class="flex flex-col" aria-label="Variables" {
							@for key in &keys {
								li class="flex items-center justify-between gap-3 border-b border-border py-2 last:border-b-0" data-env-key=(key) {
									span class="min-w-0 truncate font-mono text-sm" { (key) }
									span class="text-sm text-muted-foreground" aria-label="value hidden" { "value hidden" }
									@if can_update {
										button type="button" class=(button_class(Variant::Ghost, Size::Sm))
											"data-on:click"=(format!("$unsetKey = {}; @post({unset_url})", js_string(key))) {
											(icon("trash-2")) span class="sr-only" { "Remove " (key) }
										}
									}
								}
							}
						}
					}
				}))
			}))
			@if can_update {
				(card(&html! {
					(card_heading("Set a variable", None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(set) {
							div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
								label class=(LABEL) {
									"Name"
									input class={ (INPUT) " font-mono" } type="text" required maxlength="128" autocomplete="off" spellcheck="false"
										placeholder="DATABASE_URL" "data-bind"="envKey";
								}
								label class=(LABEL) {
									"Value"
									input id="env-value" class=(INPUT) type="password" autocomplete="off" "data-bind"="envValue";
								}
							}
							(form_error())
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save variable" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(frame::page(&ctx, &l, Tab::Environment, &content))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct SetSignals {
	#[serde(rename = "envKey")]
	key: String,
	#[serde(rename = "envValue")]
	value: String,
}

pub(super) async fn set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	ReadSignals(sig): ReadSignals<SetSignals>,
) -> Response {
	let key = sig.key.trim().to_owned();
	let result = service::update(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		id,
		UpdateInput {
			env_set: [(key, sig.value)].into_iter().collect(),
			..UpdateInput::default()
		},
	)
	.await;
	match result {
		Ok(_) => sse::reload().into_response(),
		// The value is not kept in the page after a refusal either.
		Err(e) => sse::error(&error::message(&e))
			.and(sse::signals(&json!({ "envValue": "" })))
			.into_response(),
	}
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct UnsetSignals {
	#[serde(rename = "unsetKey")]
	key: String,
}

pub(super) async fn unset(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	ReadSignals(sig): ReadSignals<UnsetSignals>,
) -> Response {
	let result = service::update(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		id,
		UpdateInput {
			env_unset: vec![sig.key],
			..UpdateInput::default()
		},
	)
	.await;
	match result {
		Ok(_) => sse::reload().into_response(),
		Err(e) => frame::fail_action(&e),
	}
}
