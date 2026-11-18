//! The Advanced tab: identifiers for the API, and the danger zone.
//!
//! Deleting an application stops and removes its containers and volumes, so it sits behind
//! a dialog that asks for the slug to be typed. The dialog itself is part of every tab's
//! frame (the header's menu opens it too); this tab holds the explanation and a button.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::create::copy_row;
use super::{Tab, forbid_unless_allowed, load, page, project_url};
use crate::applications::service;
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	Size, Variant, button_class, card, card_content, card_section, dialog_open_button, icon,
};
use crate::ui::extract::TeamCtx;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/advanced",
			get(advanced_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/delete",
			post(remove),
		)
}

async fn advanced_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Advanced)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let can_delete = ctx.perms.can(Action::Delete, Resource::Application);
	let content = html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_section("Identifiers", Some("What the JSON API and the webhook address this application by."), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						(copy_row("Application ID", &app.id.to_string()))
						(copy_row("Slug", &app.slug))
					}
				}))
			}))
			@if can_delete {
				div class="ui-danger-zone" {
					(card(&html! {
						(card_section("Danger zone", None, None))
						(card_content(&html! {
							div class="flex flex-wrap items-center justify-between gap-4" {
								div class="flex max-w-prose flex-col gap-1" {
									p class="text-sm font-medium" { "Delete this application" }
									p class="text-sm text-muted-foreground" {
										"Stops and removes its containers and volumes, and its history. You will be asked to type "
										strong class="font-mono" { (app.slug) } " to confirm."
									}
								}
								(dialog_open_button(
									"delete-dialog",
									&button_class(Variant::Destructive, Size::Md),
									&html! { (icon("trash-2")) "Delete application" },
								))
							}
						}))
					}))
				}
			}
		}
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Advanced, &content).await)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DeleteSignals {
	confirm: String,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let result = async {
		// Check the slug after a read, so a mistyped one never reaches the delete; `remove`
		// still authorizes the delete itself.
		let found = service::get(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		if sig.confirm != found.slug {
			return Ok(None);
		}
		service::remove(&s, ctx.user.id, ctx.team.id, project, app).await?;
		Ok::<_, ServerError>(Some(found.environment_id))
	}
	.await;
	match result {
		Ok(Some(environment)) => sse::redirect(&format!(
			"{}?environment={environment}",
			project_url(ctx.team.id, project)
		))
		.into_response(),
		Ok(None) => sse::signals(&json!({
			"_delete_error": "The slug doesn't match. Type it exactly to confirm.",
		}))
		.into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}
