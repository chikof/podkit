//! `/teams/{t}/models`: the team's models and downloads, as an Explore surface: a grid of
//! logo cards for the models, a table for the weights fetched onto servers.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};

use super::{catalog_url, crumbs, download_url, human_bytes, model_url, models_url};
use crate::auth::permission::{Action, Resource};
use crate::ids::ProjectId;
use crate::model_files::{ModelFile, ModelFileStatus};
use crate::models::engines;
use crate::models::service as models_service;
use crate::projects::overview::model_mark;
use crate::state::AppState;
use crate::ui::components::{
	LogoSize, Size, Variant, button_class, empty_state, icon, link_card, logo, progress, table,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::app_page;
use crate::ui::status::{Status, status_badge};
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

/// A native progress bar and the figures under it.
pub(super) fn progress_bar(file: &ModelFile) -> Markup {
	let pct = percent(file);
	let (done, total) = (
		u64::try_from(file.downloaded_bytes).unwrap_or(0),
		u64::try_from(file.total_bytes).unwrap_or(0),
	);
	progress(
		&format!("Download of {}", file.repo),
		pct,
		&format!(
			"{} of {} · {pct}%",
			human_bytes(done.min(total)),
			human_bytes(total)
		),
	)
}

fn model_cards(
	ctx: &TeamCtx,
	rows: &[crate::models::Model],
	project_names: &HashMap<ProjectId, String>,
) -> Markup {
	html! {
		div class="ui-card-grid" {
			@for m in rows {
				@let description = format!(
					"{} · {}",
					project_names.get(&m.project_id).map_or("", String::as_str),
					m.served_name
				);
				(link_card(
					&model_url(ctx.team.id, m.project_id, m.id),
					&logo(model_mark(m.engine), LogoSize::Md),
					&m.name,
					Some(&description),
					Some(&html! {
						(status_badge(m.status))
						span class="text-xs text-muted-foreground" { (engines::spec(m.engine).label) }
					}),
				))
			}
		}
	}
}

fn downloads_table(ctx: &TeamCtx, files: &[ModelFile]) -> Markup {
	let rows: Vec<Vec<Markup>> = files
		.iter()
		.map(|f| {
			vec![
				html! {
					a class="font-medium break-all hover:underline" href=(download_url(ctx.team.id, f.id)) { (f.repo) }
				},
				status_badge(f.status),
				if f.status.in_flight() {
					progress_bar(f)
				} else {
					html! { (human_bytes(u64::try_from(f.total_bytes).unwrap_or(0))) }
				},
			]
		})
		.collect();
	table(
		"downloads",
		"Downloads",
		&["Repository", "Status", "Size"],
		&rows,
	)
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Response, Response> {
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
	let browse = html! { (icon("search")) "Browse the catalog" };
	let body = html! {
		div class="ui-wide flex flex-col gap-8" {
			div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-3" {
				div class="flex flex-col gap-1" {
					h1 class="text-2xl font-semibold tracking-tight" { "Models" }
					p class="max-w-2xl text-sm text-muted-foreground" {
						"Inference engines running on your servers. Each one serves an OpenAI-compatible API to the applications you link it to."
					}
				}
				div class="flex flex-wrap gap-2" {
					@if can_download {
						a class=(button_class(Variant::Default, Size::Md)) href=(catalog_url(ctx.team.id)) { (browse) }
					}
					a class=(button_class(Variant::Outline, Size::Md)) href={ (models_url(ctx.team.id)) "/settings" } {
						(icon("settings")) "Settings"
					}
				}
			}
			@if rows.is_empty() && files.is_empty() {
				(empty_state(
					"brain",
					"No models yet",
					Some("Pick one from the catalog to download and start it."),
					can_download.then(|| html! {
						a class=(button_class(Variant::Default, Size::Md)) href=(catalog_url(ctx.team.id)) { (browse) }
					}).as_ref(),
				))
			} @else {
				section class="flex flex-col gap-3" aria-labelledby="models-heading" {
					h2 id="models-heading" class="text-lg font-semibold" { "Your models" }
					@if rows.is_empty() {
						p class="text-sm text-muted-foreground" { "No model is set up yet. Downloaded weights below can start one." }
					} @else {
						(model_cards(&ctx, &rows, &project_names))
					}
				}
				section class="flex flex-col gap-3" aria-labelledby="downloads-heading" {
					h2 id="downloads-heading" class="text-lg font-semibold" { "Downloads" }
					p class="text-sm text-muted-foreground" { "Weights fetched from Hugging Face onto a server. A model serves one of these." }
					@if files.is_empty() {
						p class="text-sm text-muted-foreground" { "Nothing downloaded yet." }
					} @else {
						(downloads_table(&ctx, &files))
					}
				}
			}
		}
	};
	Ok(app_page(&ctx, "Models", &crumbs(&ctx, vec![]), &body).into_response())
}
