//! A project's environment: tabs for its environments, a grid of resource cards (logo,
//! name, type, status, URL, last deployment), a type filter and search, and the picker that
//! creates any kind of resource.
//!
//! An Operate surface: the person is here to open or act on a resource, so the page is the
//! grid and its one primary action is "Create". `/projects/{p}` shows the default
//! environment; `/projects/{p}/{env_slug}` a named one.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use tower_cookies::Cookies;

use super::cards::{card_logo, in_flight, relative_time, status_line};
use super::picker;
use crate::auth::permission::{Action, Resource};
use crate::environments;
use crate::ids::{EnvironmentId, ProjectId, TeamId};
use crate::projects::overview::{self, EnvironmentView, Pick, ResourceCard, ResourceKind};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, Dismiss, INPUT, LABEL, MENU_ITEM, MENU_ITEM_DANGER, SELECT, Size, TabItem, Tone,
	Variant, badge, button_class, dialog, dialog_cancel, dialog_open_button, dropdown_menu,
	empty_state, icon, menu_label, tabs,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::pages::apps::{app_url, public_link};
use crate::ui::sse::js_string;
use crate::ui::status::poll;
use crate::ui::{error, shell, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/projects/{project_id}", get(default_page))
		.route(
			"/teams/{team_id}/projects/{project_id}/{env_slug}",
			get(env_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/{env_slug}/resources",
			get(resources),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments",
			post(create_environment),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/rename",
			post(rename_environment),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/delete",
			post(delete_environment),
		)
}

fn project_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}")
}

fn env_url(team: TeamId, project: ProjectId, slug: &str) -> String {
	format!("{}/{slug}", project_url(team, project))
}

fn crumbs<'a>(ctx: &TeamCtx, view: &'a EnvironmentView) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(view.project.name.as_str(), None),
	]
}

// --- the filter ----------------------------------------------------------------------

/// The search text and type filter, from the query string of a page load or the signals
/// of a Datastar request.
#[derive(Debug, Default, Clone)]
struct Filter {
	q: String,
	kind: Option<ResourceKind>,
}

impl Filter {
	fn new(q: &str, kind: &str) -> Self {
		Self {
			q: q.trim().to_owned(),
			kind: ResourceKind::from_slug(kind),
		}
	}

	fn is_active(&self) -> bool {
		!self.q.is_empty() || self.kind.is_some()
	}

	fn keeps(&self, card: &ResourceCard) -> bool {
		let q = self.q.to_lowercase();
		self.kind.is_none_or(|k| k == card.kind)
			&& (q.is_empty()
				|| card.name.to_lowercase().contains(&q)
				|| card.detail.to_lowercase().contains(&q))
	}
}

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
	#[serde(default)]
	environment: Option<i64>,
	#[serde(default)]
	q: String,
	#[serde(default, rename = "type")]
	kind: String,
}

#[derive(Debug, Default, Deserialize)]
struct FilterSignals {
	#[serde(default)]
	q: String,
	#[serde(default, rename = "type")]
	kind: String,
}

// --- the grid ------------------------------------------------------------------------

fn resource_href(team: TeamId, project: ProjectId, card: &ResourceCard) -> String {
	match card.kind {
		ResourceKind::Application => app_url(team, project, crate::ids::ApplicationId(card.id)),
		ResourceKind::Database => format!("{}/databases/{}", project_url(team, project), card.id),
		ResourceKind::Model => format!("{}/models/{}", project_url(team, project), card.id),
		ResourceKind::Stack => format!("{}/stacks/{}", project_url(team, project), card.id),
	}
}

fn tile(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	card: &ResourceCard,
	now: OffsetDateTime,
) -> Markup {
	let href = resource_href(ctx.team.id, project, card);
	let when = match (card.kind, card.changed_at) {
		(ResourceKind::Application, Some(at)) => Some(("Deployed ", at)),
		(ResourceKind::Model | ResourceKind::Stack, Some(at)) => Some(("Updated ", at)),
		_ => None,
	};
	html! {
		article class="ui-card ui-link-card flex flex-col gap-4 rounded-xl border border-border bg-card p-5 text-card-foreground shadow-xs"
			data-kind=(card.kind.slug()) data-resource=(card.id) {
			div class="flex items-start gap-3" {
				(card_logo(card))
				div class="flex min-w-0 flex-1 flex-col gap-1" {
					div class="flex items-center justify-between gap-2" {
						h3 class="min-w-0 truncate text-sm leading-tight font-semibold" {
							a href=(href) class="ui-link-card-link outline-hidden" { (card.name) }
						}
						span class="shrink-0" { (badge(Tone::Neutral, card.kind.label())) }
					}
					p class="truncate text-xs text-muted-foreground" { (card.detail) }
				}
			}
			div class="mt-auto flex flex-col gap-2" {
				div class="flex flex-wrap items-center justify-between gap-x-3 gap-y-1" {
					(status_line(card.status))
					@if let Some((verb, at)) = when {
						p class="text-xs text-muted-foreground" { (verb) (relative_time(at, now)) }
					}
				}
				@if let Some(host) = &card.host {
					p class="truncate text-xs" {
						a href=(public_link(s, host)) class="inline-flex max-w-full items-center gap-1 text-muted-foreground hover:text-foreground hover:underline"
							target="_blank" rel="noopener noreferrer" {
							span class="truncate" { (host) }
							(icon("external-link"))
						}
					}
				}
			}
		}
	}
}

/// The region that polling and the filter re-render: the grid, or what stands in for it.
fn grid(
	s: &AppState,
	ctx: &TeamCtx,
	view: &EnvironmentView,
	filter: &Filter,
	now: OffsetDateTime,
) -> Markup {
	let shown: Vec<&ResourceCard> = view.resources.iter().filter(|c| filter.keeps(c)).collect();
	let busy = shown.iter().any(|c| in_flight(c.status));
	let fragment = fragment_url(ctx.team.id, view.project.id, &view.environment.slug);
	html! {
		div id="resource-grid" class="flex flex-col gap-4" {
			(poll(&fragment, busy))
			@if view.resources.is_empty() {
				(empty_state(
					"boxes",
					&format!("Nothing in {} yet", view.environment.name),
					Some("Create an application, a database or a model. Each one shows up here with its status."),
					picker::can_create_any(ctx).then(picker::open_button).as_ref(),
				))
			} @else if shown.is_empty() {
				(empty_state(
					"search",
					"Nothing matches",
					Some("No resource in this environment fits the search and the type you picked."),
					Some(&html! {
						button type="button" class=(button_class(Variant::Outline, Size::Md))
							"data-on:click"=(format!("$q = ''; $type = 'all'; @get({})", js_string(&fragment))) {
							"Clear the filter"
						}
					}),
				))
			} @else {
				p class="text-sm text-muted-foreground" aria-live="polite" {
					(shown.len()) @if shown.len() == 1 { " resource" } @else { " resources" }
					@if filter.is_active() { " match" }
				}
				div class="ui-card-grid" {
					@for card in &shown {
						(tile(s, ctx, view.project.id, card, now))
					}
				}
			}
		}
	}
}

fn fragment_url(team: TeamId, project: ProjectId, slug: &str) -> String {
	format!("/teams/{team}/projects/{project}/{slug}/resources")
}

fn filter_bar(view: &EnvironmentView, ctx: &TeamCtx, filter: &Filter) -> Markup {
	let page = env_url(ctx.team.id, view.project.id, &view.environment.slug);
	let fragment = fragment_url(ctx.team.id, view.project.id, &view.environment.slug);
	let get = format!("@get({})", js_string(&fragment));
	let kind = filter.kind.map_or("all", ResourceKind::slug);
	html! {
		form method="get" action=(page) role="search" class="flex flex-wrap items-center gap-2"
			"data-on:submit"=(format!("evt.preventDefault(); {get}")) {
			div class="relative min-w-48 flex-1" {
				span class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground" { (icon("search")) }
				input type="search" name="q" value=(filter.q) class={ (INPUT) " pl-9" } autocomplete="off"
					aria-label="Search resources" placeholder="Search resources"
					"data-bind:q" "data-on:input__debounce.250ms"=(get);
			}
			select name="type" class={ (SELECT) " sm:w-44" } aria-label="Filter by type"
				"data-bind:type" "data-on:change"=(get) {
				button { selectedcontent {} }
				@for (value, label) in [("all", "All types"), ("application", "Applications"), ("database", "Databases"), ("model", "Models"), ("stack", "Stacks")] {
					option value=(value) selected[value == kind] { (label) }
				}
			}
			noscript { button type="submit" class=(button_class(Variant::Outline, Size::Md)) { "Filter" } }
		}
	}
}

// --- tabs, menus, dialogs ------------------------------------------------------------

fn tab_row(ctx: &TeamCtx, view: &EnvironmentView) -> Markup {
	let items: Vec<(String, String, bool)> = view
		.environments
		.iter()
		.map(|e| {
			(
				e.name.clone(),
				env_url(ctx.team.id, view.project.id, &e.slug),
				e.id == view.environment.id,
			)
		})
		.collect();
	let items: Vec<TabItem<'_>> = items
		.iter()
		.map(|(label, href, current)| TabItem {
			label,
			href,
			current: *current,
		})
		.collect();
	let can_create = ctx.perms.can(Action::Create, Resource::Project);
	let can_update = ctx.perms.can(Action::Update, Resource::Project);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Project);
	html! {
		div class="flex items-center gap-2" {
			div class="min-w-0 flex-1" { (tabs("Environments", &items)) }
			@if can_create {
				(dialog_open_button(
					"environment-create",
					&button_class(Variant::Ghost, Size::Icon),
					&html! { (icon("plus")) span class="sr-only" { "New environment" } },
				))
			}
			@if can_update || can_delete {
				(dropdown_menu(
					"environment-menu",
					&button_class(Variant::Ghost, Size::Icon),
					&format!("Actions for the {} environment", view.environment.name),
					&icon("ellipsis"),
					&html! {
						(menu_label("Environment"))
						@if can_update {
							button type="button" class=(MENU_ITEM) command="show-modal" commandfor="environment-rename"
								"data-on:click"="$_error = ''; el.commandForElement || document.getElementById('environment-rename').showModal()" {
								(icon("pencil")) "Rename"
							}
						}
						@if can_delete {
							button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor="environment-delete"
								"data-on:click"="$_error = ''; el.commandForElement || document.getElementById('environment-delete').showModal()" {
								(icon("trash-2")) "Delete"
							}
						}
					},
				))
			}
		}
	}
}

fn actions_url(team: TeamId, project: ProjectId, action: &str) -> String {
	format!("{}/environments{action}", project_url(team, project))
}

fn create_env_dialog(team: TeamId, project: ProjectId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&actions_url(team, project, ""))
	);
	dialog(
		"environment-create",
		"New environment",
		Some(
			"An environment holds its own applications, databases and models, such as staging next to production.",
		),
		Dismiss::Esc,
		&html! {
			form id="environment-create-form" class="flex flex-col gap-4" "data-on:submit"=(post) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus
						placeholder="e.g. Staging" "data-bind"="newName"
						"data-on:input"="$newSlug = $newName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60)";
				}
				label class=(LABEL) {
					"Slug"
					input class={ (INPUT) " font-mono" } type="text" required maxlength="60" autocomplete="off"
						placeholder="e.g. staging" "data-bind"="newSlug";
				}
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="environment-create-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create environment" }
		},
	)
}

fn rename_env_dialog(team: TeamId, project: ProjectId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&actions_url(team, project, "/rename"))
	);
	dialog(
		"environment-rename",
		"Rename environment",
		Some("Its slug and the links to it stay the same."),
		Dismiss::Esc,
		&html! {
			form id="environment-rename-form" class="flex flex-col gap-4" "data-on:submit"=(post) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus "data-bind"="envName";
				}
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="environment-rename-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save name" }
		},
	)
}

fn delete_env_dialog(team: TeamId, project: ProjectId, name: &str) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&actions_url(team, project, "/delete"))
	);
	dialog(
		"environment-delete",
		"Delete environment",
		None,
		Dismiss::Esc,
		&html! {
			form id="environment-delete-form" class="flex flex-col gap-4" "data-on:submit"=(post) {
				p {
					"Delete the " strong { (name) } " environment? An environment can only be deleted once it is empty, and a project keeps at least one."
				}
				(form_error())
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Cancel" }
			}
			button type="submit" form="environment-delete-form" class=(BUTTON_DANGER)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Delete environment" }
		},
	)
}

// --- pages ---------------------------------------------------------------------------

async fn render(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	pick: Pick<'_>,
	filter: &Filter,
) -> Result<Markup, Response> {
	let view = overview::environment_view(&s.db, ctx.user.id, ctx.team.id, project, pick)
		.await
		.map_err(|e| error::page(ctx, &e))?;
	let now = OffsetDateTime::now_utc();
	let can_pick = picker::can_create_any(ctx);
	let can_create_env = ctx.perms.can(Action::Create, Resource::Project);
	let can_update = ctx.perms.can(Action::Update, Resource::Project);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Project);
	let signals = json!({
		"q": filter.q,
		"type": filter.kind.map_or("all", ResourceKind::slug),
		"newName": "",
		"newSlug": "",
		"envName": view.environment.name,
		"envId": view.environment.id.to_string(),
		"_error": "",
	});

	let body = html! {
		div class="ui-wide flex w-full flex-col gap-6" data-signals=(signals.to_string()) {
			div class="flex flex-wrap items-end justify-between gap-4" {
				div class="flex min-w-0 flex-col gap-1" {
					h1 class="truncate text-2xl font-semibold tracking-tight" { (view.project.name) }
					p class="truncate font-mono text-xs text-muted-foreground" { (view.project.slug) }
				}
				@if can_pick { (picker::open_button()) }
			}
			(tab_row(ctx, &view))
			(filter_bar(&view, ctx, filter))
			(grid(s, ctx, &view, filter, now))
			@if can_pick { (picker::dialog(ctx, view.project.id, &view.environment)) }
			@if can_create_env { (create_env_dialog(ctx.team.id, view.project.id)) }
			@if can_update { (rename_env_dialog(ctx.team.id, view.project.id)) }
			@if can_delete { (delete_env_dialog(ctx.team.id, view.project.id, &view.environment.name)) }
		}
	};
	Ok(app_page(
		ctx,
		&view.project.name,
		&crumbs(ctx, &view),
		&body,
	))
}

async fn default_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<PageQuery>,
) -> Result<Markup, Response> {
	let pick = q
		.environment
		.map_or(Pick::Default, |id| Pick::Id(EnvironmentId(id)));
	render(&s, &ctx, project, pick, &Filter::new(&q.q, &q.kind)).await
}

async fn env_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, slug)): Path<(TeamId, ProjectId, String)>,
	Query(q): Query<PageQuery>,
) -> Result<Markup, Response> {
	render(
		&s,
		&ctx,
		project,
		Pick::Slug(&slug),
		&Filter::new(&q.q, &q.kind),
	)
	.await
}

/// The grid alone, for the filter and for polling.
async fn resources(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, slug)): Path<(TeamId, ProjectId, String)>,
	ReadSignals(sig): ReadSignals<Option<FilterSignals>>,
) -> Response {
	let sig = sig.unwrap_or_default();
	match overview::environment_view(&s.db, ctx.user.id, ctx.team.id, project, Pick::Slug(&slug))
		.await
	{
		Ok(view) => {
			let filter = Filter::new(&sig.q, &sig.kind);
			sse::patch(&grid(&s, &ctx, &view, &filter, OffsetDateTime::now_utc())).into_response()
		}
		// A real 404 (not an SSE message): a poll that lost its environment stops.
		Err(e) => error::page(&ctx, &e),
	}
}

// --- environment actions -------------------------------------------------------------

#[derive(Deserialize)]
struct CreateSignals {
	#[serde(rename = "newName")]
	name: String,
	#[serde(rename = "newSlug")]
	slug: String,
}

async fn create_environment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	match environments::service::create(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		project,
		&sig.name,
		&sig.slug,
	)
	.await
	{
		Ok(env) => {
			shell::flash(&cookies, Tone::Success, &format!("Created {}.", env.name));
			sse::redirect(&env_url(ctx.team.id, project, &env.slug)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct RenameSignals {
	#[serde(rename = "envId")]
	id: String,
	#[serde(rename = "envName")]
	name: String,
}

async fn rename_environment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<RenameSignals>,
) -> Response {
	let Ok(id) = sig.id.parse::<i64>().map(EnvironmentId) else {
		return sse::error("Pick an environment to rename.").into_response();
	};
	match environments::service::rename(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		project,
		id,
		&sig.name,
		None,
	)
	.await
	{
		Ok(env) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("Renamed to {}.", env.name),
			);
			sse::redirect(&env_url(ctx.team.id, project, &env.slug)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct DeleteSignals {
	#[serde(rename = "envId")]
	id: String,
}

async fn delete_environment(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let Ok(id) = sig.id.parse::<i64>().map(EnvironmentId) else {
		return sse::error("Pick an environment to delete.").into_response();
	};
	// The refusal for an environment that still has resources is a `Conflict` that names
	// them; `error::action` puts it in the dialog's `_error` line.
	match environments::service::remove(&s.db, ctx.user.id, ctx.team.id, project, id).await {
		Ok(env) => {
			shell::flash(&cookies, Tone::Success, &format!("Deleted {}.", env.name));
			sse::redirect(&project_url(ctx.team.id, project)).into_response()
		}
		Err(e) => error::action(&e),
	}
}
