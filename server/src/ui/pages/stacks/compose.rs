//! The Compose tab: the file as stored, editable by those who may update the stack. A save
//! is checked against the stack's variables first (`stacks::service::update`), so a file
//! that could not deploy is refused with every offending key and nothing is stored.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::frame::{self, Loaded, Tab};
use super::issues::{issues_of, placeholder, problems};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ProjectId, StackId, TeamId};
use crate::stacks::service::{self, UpdateInput};
use crate::state::AppState;
use crate::ui::components::{BUTTON, TEXTAREA, card, card_content, card_header};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::{error, sse};

fn saved_panel() -> Markup {
	html! {
		div id="compose-result" role="status" class="rounded-lg border border-border p-4 text-sm" data-state="saved" {
			"Saved. Deploy the stack to apply the change."
		}
	}
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	Ok(frame::page(&ctx, &l, Tab::Compose, &content(&ctx, &l)))
}

fn content(ctx: &TeamCtx, l: &Loaded) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Stack);
	let yaml = &l.stack().compose_yaml;
	let post = format!(
		"evt.preventDefault(); @post({})",
		crate::ui::sse::js_string(&format!("{}/compose", l.base(ctx.team.id)))
	);
	let signals = json!({ "compose": yaml, "_error": "" });
	html! {
		div class="flex flex-col gap-6" data-signals=(signals.to_string()) {
			(card(&html! {
				(card_header("Compose file", Some(if can_update {
					"Edit and save. podkit checks the whole file first and refuses a change it could not deploy."
				} else {
					"The file this stack runs."
				})))
				(card_content(&html! {
					form class="flex flex-col gap-4" "data-on:submit"=(post) {
						label class="flex flex-col gap-2 text-sm font-medium" {
							span class="sr-only" { "docker-compose.yml" }
							@if can_update {
								textarea class={ (TEXTAREA) " min-h-96" } spellcheck="false" autocomplete="off" wrap="off"
									"data-bind:compose" {}
							} @else {
								textarea class={ (TEXTAREA) " min-h-96" } readonly spellcheck="false" wrap="off" { (yaml) }
							}
						}
						(placeholder("compose-result"))
						(form_error())
						@if can_update {
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save compose file" }
							}
						}
					}
				}))
			}))
			@if !l.detail.warnings.is_empty() {
				(card(&html! {
					(card_header("Notes", Some("podkit ignores or adjusts these settings when it deploys.")))
					(card_content(&html! {
						ul class="flex list-disc flex-col gap-1 pl-5 text-sm text-muted-foreground" {
							@for w in &l.detail.warnings {
								li {
									@if let Some(svc) = &w.service { span class="font-mono text-xs" { (svc) } " " }
									code class="font-mono text-xs" { (w.path) } ": " (w.message)
								}
							}
						}
					}))
				}))
			}
		}
	}
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Signals {
	compose: String,
}

pub(super) async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = service::update(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		id,
		UpdateInput {
			compose_yaml: Some(sig.compose),
			..UpdateInput::default()
		},
	)
	.await;
	match result {
		Ok(_) => sse::patch(&saved_panel())
			.and(sse::error(""))
			.into_response(),
		Err(e) => match issues_of(&e) {
			Some((headline, rows)) => sse::patch(&problems("compose-result", &headline, &rows))
				.and(sse::error(""))
				.into_response(),
			None => error::action(&e),
		},
	}
}
