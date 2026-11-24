//! The Advanced tab: identifiers for the API, and the danger zone.
//!
//! Deleting a database stops and removes its container, and with a purge its data volume
//! too, so it sits behind a dialog that asks for the name to be typed. The dialog is part of
//! every tab's frame (the header's menu opens it as well); this tab holds the explanation and
//! a button.

use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use maud::{Markup, html};

use super::{Tab, copy_row, forbid_unless_allowed, load, page};
use crate::components::{
	Size, Variant, button_class, card, card_content, card_section, dialog_open_button, icon,
};
use crate::extract::TeamCtx;
use axum::response::Response;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{DatabaseId, ProjectId, TeamId};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/databases/{database_id}/advanced",
		get(advanced_page),
	)
}

async fn advanced_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Advanced)?;
	let (project, d) = load(&s, &ctx, project, database).await?;
	let database = &d.database;
	let can_delete = ctx.perms.can(Action::Delete, Resource::Database);
	let open = "$confirm = ''; $purge = false; $_delete_error = ''";
	let content = html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_section("Identifiers", Some("What the JSON API addresses this database by."), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						(copy_row("Database ID", &database.id.to_string()))
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
									p class="text-sm font-medium" { "Delete this database" }
									p class="text-sm text-muted-foreground" {
										"Stops and removes its container. The data volume stays on the server unless you also choose to delete it. You will be asked to type "
										strong class="font-mono break-all" { (database.name) } " to confirm."
									}
								}
								span "data-on:click"=(open) {
									(dialog_open_button(
										"delete-dialog",
										&button_class(Variant::Destructive, Size::Md),
										&html! { (icon("trash-2")) "Delete database" },
									))
								}
							}
						}))
					}))
				}
			}
		}
	};
	Ok(page(&s, &ctx, &project, &d, Tab::Advanced, &content).await)
}
