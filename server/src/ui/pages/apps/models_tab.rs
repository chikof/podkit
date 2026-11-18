//! The Models tab: the models this application reaches through the gateway.
//!
//! Linking is done from the model's own page (it owns the key, the served name and the
//! preview of what changes), so this tab lists the links and leads there.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};

use super::{Tab, load, page, project_url};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::links::service as links;
use crate::projects::overview::ResourceStatus;
use crate::state::AppState;
use crate::ui::components::{
	LogoSize, Size, Tone, Variant, badge, card, card_content, card_section, empty_state, icon,
	link_button, logo,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::projects::cards::status_line;

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/models",
		get(models_page),
	)
}

async fn models_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	super::forbid_unless_allowed(&ctx, Tab::Models)?;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let linked = links::list(&s, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let models = format!("{}/models", project_url(ctx.team.id, project.id));
	let can_browse = ctx.perms.can(Action::Create, Resource::Model);
	let catalog = format!(
		"/teams/{}/models/catalog?project={}",
		ctx.team.id, project.id
	);
	let content = card(&html! {
		(card_section(
			"Models",
			Some("A linked model is reached through the gateway with this application's own key. Link or unlink one from the model's page; changes apply on the next deploy."),
			None,
		))
		(card_content(&html! {
			@if linked.is_empty() {
				(empty_state(
					"brain",
					"No model is linked to this application.",
					Some("Open a model and link this application to let it call the model through the gateway."),
					can_browse.then(|| link_button(Variant::Outline, Size::Md, &catalog, &html! { "Browse the model catalog" })).as_ref(),
				))
			} @else {
				ul class="flex flex-col" aria-label="Linked models" {
					@for link in &linked {
						li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 first:border-t-0 first:pt-0" {
							div class="flex min-w-0 items-center gap-3" {
								(logo("huggingface", LogoSize::Sm))
								div class="flex min-w-0 flex-col gap-0.5" {
									a class="truncate text-sm font-medium hover:underline" href={ (models) "/" (link.model_id) } { (link.name) }
									span class="truncate font-mono text-xs text-muted-foreground" { "served as " (link.served_name) }
								}
							}
							div class="flex flex-wrap items-center gap-3" {
								@if link.is_default { (badge(Tone::Neutral, "default")) }
								(status_line(ResourceStatus::Model(link.status)))
								a class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground hover:underline" href={ (models) "/" (link.model_id) } {
									"Manage" (icon("arrow-up-right"))
								}
							}
						}
					}
				}
			}
		}))
	});
	Ok(page(&s, &ctx, &project, &app, Tab::Models, &content).await)
}
