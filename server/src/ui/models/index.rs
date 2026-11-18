//! `/teams/{t}/models`: the team's models and downloads.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use maud::{Markup, html};

use super::{catalog_url, crumbs, download_url, human_bytes, model_url};
use crate::auth::permission::{Action, Resource};
use crate::ids::ProjectId;
use crate::model_files::{ModelFile, ModelFileStatus};
use crate::models::engines;
use crate::models::service as models_service;
use crate::state::AppState;
use crate::ui::components::{BUTTON, BUTTON_OUTLINE, card, card_content, card_header};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::app_page;
use crate::ui::status::{Status, status_badge};
use crate::ui::{error, sse};
use crate::{model_files, projects};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/models", get(page))
}

/// The percentage a download has reached, 0 to 100.
pub(super) fn percent(file: &ModelFile) -> u64 {
	let (done, total) = (
		u64::try_from(file.downloaded_bytes).unwrap_or(0),
		u64::try_from(file.total_bytes).unwrap_or(0),
	);
	if file.status == ModelFileStatus::Ready {
		100
	} else {
		(done.min(total) * 100).checked_div(total).unwrap_or(0)
	}
}

/// A bar and the figures under it.
pub(super) fn progress_bar(file: &ModelFile) -> Markup {
	let pct = percent(file);
	let (done, total) = (
		u64::try_from(file.downloaded_bytes).unwrap_or(0),
		u64::try_from(file.total_bytes).unwrap_or(0),
	);
	html! {
		div class="flex flex-col gap-1" {
			div class="h-2 w-full overflow-hidden rounded-full bg-border" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow=(pct) {
				div class="h-full bg-primary" style={ "width: " (pct) "%" } {}
			}
			span class="text-xs text-muted-foreground" {
				(human_bytes(done.min(total))) " of " (human_bytes(total)) " · " (pct) "%"
			}
		}
	}
}

fn models_card(
	ctx: &TeamCtx,
	rows: &[crate::models::Model],
	project_names: &HashMap<ProjectId, String>,
) -> Markup {
	card(&html! {
		(card_header("Models", Some("Inference engines running on your servers. Each one serves an OpenAI-compatible API to the apps you link it to.")))
		(card_content(&html! {
			@if rows.is_empty() {
				p class="text-sm text-muted-foreground" { "No models yet. Pick one from the catalog to download and start it." }
			} @else {
				ul {
					@for m in rows {
						li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 text-sm first:border-t-0" {
							div class="flex min-w-0 flex-col gap-1" {
								a class="font-medium hover:underline" href=(model_url(ctx.team.id, m.project_id, m.id)) { (m.name) }
								span class="text-muted-foreground" {
									(project_names.get(&m.project_id).map_or("", String::as_str))
									" · " (engines::spec(m.engine).label)
									" · " (m.served_name)
								}
							}
							(status_badge(m.status))
						}
					}
				}
			}
		}))
	})
}

fn downloads_card(ctx: &TeamCtx, files: &[ModelFile]) -> Markup {
	card(&html! {
		(card_header("Downloads", Some("Weights fetched from Hugging Face onto a server. A model serves one of these.")))
		(card_content(&html! {
			@if files.is_empty() {
				p class="text-sm text-muted-foreground" { "Nothing downloaded yet." }
			} @else {
				ul {
					@for f in files {
						li class="flex flex-col gap-2 border-t border-border py-3 text-sm first:border-t-0" {
							div class="flex flex-wrap items-center justify-between gap-3" {
								a class="font-medium hover:underline" href=(download_url(ctx.team.id, f.id)) { (f.repo) }
								(status_badge(f.status))
							}
							@if f.status.in_flight() { (progress_bar(f)) }
							@else {
								span class="text-muted-foreground" { (human_bytes(u64::try_from(f.total_bytes).unwrap_or(0))) }
							}
						}
					}
				}
			}
		}))
	})
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Response, Response> {
	use axum::response::IntoResponse;
	let rows = models_service::list_team(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let files = if ctx.perms.can(Action::Read, Resource::ModelFile) {
		model_files::service::list(&s, ctx.user.id, ctx.team.id)
			.await
			.map_err(|e| error::page(&ctx, &e))?
	} else {
		Vec::new()
	};
	let project_names: HashMap<ProjectId, String> =
		projects::service::list(&s.db, ctx.user.id, ctx.team.id)
			.await
			.unwrap_or_default()
			.into_iter()
			.map(|p| (p.project.id, p.project.name))
			.collect();

	let can_download = ctx.perms.can(Action::Create, Resource::ModelFile);
	let body = html! {
		div class="flex flex-wrap items-center justify-between gap-3" {
			h1 class="text-xl font-semibold" { "Models" }
			div class="flex gap-3" {
				@if can_download {
					a class=(BUTTON) href=(catalog_url(ctx.team.id)) { "Browse the catalog" }
				}
				a class=(BUTTON_OUTLINE) href={ (super::models_url(ctx.team.id)) "/settings" } { "Settings" }
			}
		}
		(models_card(&ctx, &rows, &project_names))
		(downloads_card(&ctx, &files))
	};
	let _ = sse::js_string;
	Ok(app_page(&ctx, "Models", &crumbs(&ctx, vec![]), &body).into_response())
}
