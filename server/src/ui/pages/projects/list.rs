//! The projects grid: one tile per project with its resource logos, counts and last
//! deploy, and the dialogs to create, rename and delete one.
//!
//! This is an Explore surface (a team browsing what it runs), so the page is the grid;
//! the single primary action is "New project".

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use time::OffsetDateTime;
use tower_cookies::Cookies;

use super::cards::{logo_row, relative_time};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ProjectId, TeamId};
use crate::projects::overview::{self, ProjectCard};
use crate::projects::service;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, Dismiss, INPUT, LABEL, MENU_ITEM, MENU_ITEM_DANGER, Size, Tone, Variant,
	button_class, dialog, dialog_cancel, dialog_open_button, dropdown_menu, empty_state, icon,
	menu_label,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::{error, shell, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/projects", get(list_page).post(create))
		.route("/teams/{team_id}/projects/rename", post(rename))
		.route("/teams/{team_id}/projects/delete", post(remove))
}

fn list_url(team: TeamId) -> String {
	format!("/teams/{team}/projects")
}

/// Where a project's card leads: its default environment, or the bare project URL (which
/// resolves to the same place) for a project that somehow has none.
fn project_href(team: TeamId, card: &ProjectCard) -> String {
	let base = format!("/teams/{team}/projects/{}", card.project.id);
	match &card.default_environment {
		Some(slug) => format!("{base}/{slug}"),
		None => base,
	}
}

fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Projects", Some(list_url(ctx.team.id)))]
}

fn plural(n: i64, one: &str, many: &str) -> String {
	format!("{n} {}", if n == 1 { one } else { many })
}

/// "2 applications · 1 database", leaving out what the project doesn't have.
fn counts_line(card: &ProjectCard) -> String {
	let mut parts = Vec::new();
	if card.applications > 0 {
		parts.push(plural(card.applications, "application", "applications"));
	}
	if card.databases > 0 {
		parts.push(plural(card.databases, "database", "databases"));
	}
	if card.models > 0 {
		parts.push(plural(card.models, "model", "models"));
	}
	if parts.is_empty() {
		"Nothing deployed yet".to_owned()
	} else {
		parts.join(" · ")
	}
}

fn card_menu(card: &ProjectCard, can_update: bool, can_delete: bool) -> Markup {
	if !can_update && !can_delete {
		return html! {};
	}
	let id = card.project.id.0;
	let menu_id = format!("project-menu-{id}");
	let label = format!("Actions for {}", card.project.name);
	html! {
		div class="ui-card-menu" {
			(dropdown_menu(
				&menu_id,
				&button_class(Variant::Ghost, Size::Icon),
				&label,
				&icon("ellipsis"),
				&html! {
					(menu_label("Project"))
					@if can_update {
						button type="button" class=(MENU_ITEM)
							command="show-modal" commandfor="project-rename"
							data-id=(id) data-name=(card.project.name)
							"data-on:click"="$target = el.dataset.id; $name = el.dataset.name; $_error = ''; el.commandForElement || document.getElementById('project-rename').showModal()" {
							(icon("pencil")) "Rename"
						}
					}
					@if can_delete {
						button type="button" class=(MENU_ITEM_DANGER)
							command="show-modal" commandfor="project-delete"
							data-id=(id) data-slug=(card.project.slug)
							"data-on:click"="$target = el.dataset.id; $targetSlug = el.dataset.slug; $confirm = ''; $_error = ''; el.commandForElement || document.getElementById('project-delete').showModal()" {
							(icon("trash-2")) "Delete"
						}
					}
				},
			))
		}
	}
}

fn project_card(
	ctx: &TeamCtx,
	card: &ProjectCard,
	now: OffsetDateTime,
	can_update: bool,
	can_delete: bool,
) -> Markup {
	let href = project_href(ctx.team.id, card);
	let total = card.logos.len();
	html! {
		article class="ui-card ui-link-card ui-proj" data-project=(card.project.id) {
			div class="ui-proj-head" {
				div class="ui-proj-title" {
					h2 class="ui-proj-name" {
						a href=(href) class="ui-link-card-link" { (card.project.name) }
					}
					p class="ui-proj-slug" { (card.project.slug) }
				}
				div class="ui-proj-side" {
					@if total == 0 {
						span class="ui-proj-none" { "No resources yet" }
					} @else {
						(logo_row(&card.logos))
					}
					(card_menu(card, can_update, can_delete))
				}
			}
			dl class="ui-proj-meta" {
				div {
					dt { "Resources" }
					dd { (counts_line(card)) }
				}
				div {
					dt { "Last deploy" }
					dd {
						@if let Some(at) = card.last_deploy {
							(relative_time(at, now))
						} @else {
							"Never deployed"
						}
					}
				}
				div {
					dt { "Environments" }
					dd { (plural(card.environments, "environment", "environments")) }
				}
			}
		}
	}
}

fn create_dialog(team: TeamId) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}')", list_url(team));
	dialog(
		"project-create",
		"New project",
		Some("A project groups the applications, databases and models that belong together."),
		Dismiss::Esc,
		&html! {
			form id="project-create-form" class="flex flex-col gap-4" "data-on:submit"=(post) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus
						placeholder="e.g. Web shop" "data-bind:name"
						"data-on:input"="$slug = $name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60)";
				}
				label class=(LABEL) {
					"Slug"
					input class={ (INPUT) " font-mono" } type="text" required maxlength="60" autocomplete="off"
						placeholder="e.g. web-shop" "data-bind:slug";
				}
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="project-create-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create project" }
		},
	)
}

fn rename_dialog(team: TeamId) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}/rename')", list_url(team));
	dialog(
		"project-rename",
		"Rename project",
		Some("The slug and the links to the project stay the same."),
		Dismiss::Esc,
		&html! {
			form id="project-rename-form" class="flex flex-col gap-4" "data-on:submit"=(post) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus "data-bind:name";
				}
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="project-rename-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save name" }
		},
	)
}

fn delete_dialog(team: TeamId) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}/delete')", list_url(team));
	dialog(
		"project-delete",
		"Delete project",
		None,
		Dismiss::Esc,
		&html! {
			form id="project-delete-form" class="flex flex-col gap-4" "data-on:submit"=(post) {
				p {
					"This deletes the project and everything in it. Type "
					strong class="font-mono" "data-text"="$targetSlug" {}
					" to confirm."
				}
				input class=(INPUT) type="text" autocomplete="off" aria-label="Project slug" "data-bind:confirm";
				(form_error())
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Cancel" }
			}
			button type="submit" form="project-delete-form" class=(BUTTON_DANGER)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Delete project" }
		},
	)
}

async fn list_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let cards = overview::cards(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Project);
	let can_update = ctx.perms.can(Action::Update, Resource::Project);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Project);
	let now = OffsetDateTime::now_utc();

	let new_button = html! {
		(dialog_open_button(
			"project-create",
			&button_class(Variant::Default, Size::Md),
			&html! { (icon("plus")) "New project" },
		))
	};
	let resources: usize = cards.iter().map(|c| c.logos.len()).sum();
	let body = html! {
		div class="ui-wide ui-proj-page" data-signals="{name: '', slug: '', target: '', targetName: '', targetSlug: '', confirm: '', _error: ''}" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Projects" }
					p class="ui-lede" { "Applications, databases and models, grouped by what they are for." }
				}
				@if can_create { div class="ui-page-actions" { (new_button) } }
			}
			@if cards.is_empty() {
				(empty_state(
					"folder",
					"No projects yet",
					Some("Create a project, then add an application, a database or a model to it."),
					can_create.then_some(&new_button),
				))
			} @else {
				ul class="ui-proj-summary" aria-label="Team summary" {
					li { b { (cards.len()) } @if cards.len() == 1 { " project" } @else { " projects" } }
					li { b { (resources) } @if resources == 1 { " resource" } @else { " resources" } }
				}
				div id="project-grid" class="ui-proj-grid ui-reveal" {
					@for (i, card) in cards.iter().enumerate() {
						div class="ui-proj-cell" style=(format!("--i:{i}")) {
							(project_card(&ctx, card, now, can_update, can_delete))
						}
					}
					@if can_create {
						div class="ui-proj-cell" style=(format!("--i:{}", cards.len())) {
							(dialog_open_button(
								"project-create",
								"ui-proj-new",
								&html! {
									(icon("plus"))
									span class="ui-proj-new-t" { "New project" }
									span class="ui-proj-new-s" { "Group resources by product" }
								},
							))
						}
					}
				}
			}
			@if can_create { (create_dialog(ctx.team.id)) }
			@if can_update { (rename_dialog(ctx.team.id)) }
			@if can_delete { (delete_dialog(ctx.team.id)) }
		}
	};
	Ok(app_page(&ctx, "Projects", &crumbs(&ctx), &body))
}

#[derive(Deserialize)]
struct CreateSignals {
	name: String,
	slug: String,
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	match service::create(&s.db, ctx.user.id, ctx.team.id, &sig.name, &sig.slug).await {
		Ok(project) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("Created {}.", project.name),
			);
			sse::redirect(&format!("/teams/{}/projects/{}", ctx.team.id, project.id))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct RenameSignals {
	target: String,
	name: String,
}

async fn rename(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<RenameSignals>,
) -> Response {
	let Ok(id) = sig.target.parse::<i64>().map(ProjectId) else {
		return sse::error("Pick a project to rename.").into_response();
	};
	match overview::rename(&s.db, ctx.user.id, ctx.team.id, id, &sig.name).await {
		Ok(project) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("Renamed to {}.", project.name),
			);
			sse::redirect(&list_url(ctx.team.id)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct DeleteSignals {
	target: String,
	confirm: String,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let Ok(id) = sig.target.parse::<i64>().map(ProjectId) else {
		return sse::error("Pick a project to delete.").into_response();
	};
	let result = async {
		// The slug is checked after a read so a mistyped slug never reaches the delete;
		// `remove` still authorizes the delete itself.
		let found = service::find(&s.db, ctx.user.id, ctx.team.id, id).await?;
		if sig.confirm != found.slug {
			return Ok(None);
		}
		Ok::<_, ServerError>(Some(
			service::remove(&s.db, ctx.user.id, ctx.team.id, id).await?,
		))
	}
	.await;
	match result {
		Ok(Some(project)) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("Deleted {}.", project.name),
			);
			sse::redirect(&list_url(ctx.team.id)).into_response()
		}
		Ok(None) => {
			sse::error("The slug doesn't match. Type it exactly to confirm.").into_response()
		}
		Err(e) => error::action(&e),
	}
}
