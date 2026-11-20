//! A project's environment: tabs for its environments, a list of resource rows (logo, name,
//! type, status, URL, last deployment; CPU and memory on hover and focus), a type filter and
//! search, and the create-resource picker page.
//!
//! An Operate surface: the person is here to open or act on a resource, so the page is the
//! list and its one primary action is "Create resource". `/projects/{p}` shows the default
//! environment; `/projects/{p}/{env_slug}` a named one.

use std::collections::HashMap;
use std::fmt::Write as _;

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

use super::cards::{card_logo, in_flight, relative_time, status_badge, status_data};
use super::picker;
use crate::auth::permission::{Action, Resource};
use crate::environments;
use crate::ids::{EnvironmentId, ProjectId, TeamId};
use crate::projects::overview::{self, EnvironmentView, Pick, ResourceCard, ResourceKind};
use crate::scanning::{Badge, BadgeState, Subject};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, INPUT, LABEL, MENU_ITEM, MENU_ITEM_DANGER, SELECT, Size, TabItem, Tone, Variant,
	button_class, dialog, dialog_cancel, dialog_open_button, dropdown_menu, empty_state, icon,
	menu_label, security_badge, skeleton_row, tabs,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::pages::apps::{app_url, public_link};
use crate::ui::pages::security;
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
			"/teams/{team_id}/projects/{project_id}/{env_slug}/create",
			get(create_page),
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
	/// What the page last showed (`kind:id=status` pairs), to tell which statuses moved.
	#[serde(default)]
	seen: String,
}

/// "Private network podkit-env-7 on this server": where the environment's resources can
/// reach each other and nothing else. Nothing until a resource of it has been deployed.
fn network_line(team: TeamId, networks: &[crate::networks::service::NetworkView]) -> Markup {
	html! {
		@for view in networks {
			p class="ui-lede" {
				"Private network " code class="font-mono" { (view.network.podman_name) } " · "
				a class="hover:underline" href={ "/teams/" (team) "/servers/" (view.network.server_id) "/networks" } {
					(view.members.len()) @if view.members.len() == 1 { " resource" } @else { " resources" }
				}
			}
		}
	}
}

/// "6 resources in production · 4 running · 1 building": the environment at a glance.
fn summary_line(view: &EnvironmentView) -> String {
	let n = view.resources.len();
	let mut line = format!(
		"{n} {} in {}",
		if n == 1 { "resource" } else { "resources" },
		view.environment.name
	);
	for key in [
		"running",
		"building",
		"deploying",
		"failed",
		"stopped",
		"pending",
	] {
		let count = view
			.resources
			.iter()
			.filter(|c| status_data(c.status) == key)
			.count();
		if count > 0 {
			let _ = write!(line, " · {count} {key}");
		}
	}
	line
}

// --- the list ------------------------------------------------------------------------

fn resource_href(team: TeamId, project: ProjectId, card: &ResourceCard) -> String {
	match card.kind {
		ResourceKind::Application => app_url(team, project, crate::ids::ApplicationId(card.id)),
		ResourceKind::Database => format!("{}/databases/{}", project_url(team, project), card.id),
		ResourceKind::Model => format!("{}/models/{}", project_url(team, project), card.id),
		ResourceKind::Stack => format!("{}/stacks/{}", project_url(team, project), card.id),
	}
}

/// The icon in front of a resource's type.
const fn kind_icon(kind: ResourceKind) -> &'static str {
	match kind {
		ResourceKind::Application => "git-branch",
		ResourceKind::Database => "database",
		ResourceKind::Model => "brain",
		ResourceKind::Stack => "layers",
	}
}

/// `412 MB`, `1.1 GB`: resident memory the way a person reads it.
fn format_memory(bytes: i64) -> String {
	const MB: f64 = 1024.0 * 1024.0;
	#[allow(clippy::cast_precision_loss)]
	let mb = bytes.max(0) as f64 / MB;
	if mb >= 1024.0 {
		format!("{:.1} GB", mb / 1024.0)
	} else {
		format!("{mb:.0} MB")
	}
}

/// The CPU and memory figures of a row: shown on hover and on focus, never hidden from a
/// screen reader. A resource with no recent sample shows dashes.
fn metrics(card: &ResourceCard) -> Markup {
	let (cpu, mem) = card.usage.map_or_else(
		|| ("—".to_owned(), "—".to_owned()),
		|u| {
			(
				u.cpu.map_or_else(|| "—".to_owned(), |c| format!("{c:.1}%")),
				format_memory(u.mem),
			)
		},
	);
	html! {
		dl class="ui-res-metrics" {
			div { dt { "CPU" } dd { (cpu) } }
			div { dt { "Mem" } dd { (mem) } }
		}
	}
}

/// What the page last showed of each resource, as `kind:id=status` pairs. The poll sends it
/// back so the server can say which statuses changed since.
fn seen_of(cards: &[ResourceCard]) -> String {
	cards
		.iter()
		.map(|c| format!("{}:{}={}", c.kind.slug(), c.id, status_data(c.status)))
		.collect::<Vec<_>>()
		.join(";")
}

/// Whether a card's status differs from the one the page last showed. A card the page has
/// not seen yet, or a first render, is not a change.
fn changed_since(seen: Option<&str>, card: &ResourceCard) -> bool {
	let Some(seen) = seen.filter(|s| !s.is_empty()) else {
		return false;
	};
	let key = format!("{}:{}=", card.kind.slug(), card.id);
	seen.split(';')
		.find_map(|pair| pair.strip_prefix(&key))
		.is_some_and(|before| before != status_data(card.status))
}

/// The security badge of each resource the list shows, by what it is. A resource never
/// scanned is not in it.
type Badges = HashMap<Subject, Badge>;

/// The thing a security badge is about: a resource card is one of the four kinds scanning
/// covers.
const fn subject_of(card: &ResourceCard) -> Subject {
	match card.kind {
		ResourceKind::Application => Subject::Application(crate::ids::ApplicationId(card.id)),
		ResourceKind::Database => Subject::Database(crate::ids::DatabaseId(card.id)),
		ResourceKind::Model => Subject::Model(crate::ids::ModelId(card.id)),
		ResourceKind::Stack => Subject::Stack(crate::ids::StackId(card.id)),
	}
}

/// One query for the whole list. Empty without `read scan`, so the rows carry no badge.
async fn badges_of(s: &AppState, ctx: &TeamCtx, view: &EnvironmentView) -> Badges {
	let subjects: Vec<Subject> = view.resources.iter().map(subject_of).collect();
	security::row_badges(s, ctx, &subjects)
		.await
		.into_iter()
		.filter(|(_, badge)| badge.state != BadgeState::None)
		.collect()
}

#[allow(
	clippy::too_many_arguments,
	reason = "one row of the list: what it shows, when, where it sits and whether it just changed"
)]
fn row(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	card: &ResourceCard,
	now: OffsetDateTime,
	index: usize,
	flip: bool,
	badges: &Badges,
) -> Markup {
	let href = resource_href(ctx.team.id, project, card);
	let subject = subject_of(card);
	let badge = badges
		.get(&subject)
		.map(|b| security_badge(b, Some(&security::tab_url(ctx.team.id, project, subject))));
	let when = match (card.kind, card.changed_at) {
		(ResourceKind::Application, Some(at)) => Some(("Deployed ", at)),
		(ResourceKind::Model | ResourceKind::Stack, Some(at)) => Some(("Updated ", at)),
		_ => None,
	};
	html! {
		li class="ui-res" data-kind=(card.kind.slug()) data-resource=(card.id)
			data-status=(status_data(card.status)) style=(format!("--i:{index}")) {
			div class="ui-res-logo" { (card_logo(card)) }
			div class="ui-res-main" {
				span class="ui-res-kind" { (icon(kind_icon(card.kind))) (card.kind.label()) @if let Some(b) = &badge { (b) } }
				h2 class="ui-res-name" {
					a href=(href) class="ui-res-link" { (card.name) }
				}
				p class="ui-res-sub" { (card.detail) }
			}
			div class="ui-res-meta" {
				@if let Some(host) = &card.host {
					a href=(public_link(s, host)) class="ui-res-url" target="_blank" rel="noopener noreferrer" {
						span { (host) }
						(icon("external-link"))
					}
				}
				@if let Some((verb, at)) = when {
					span class="ui-res-when" { (verb) (relative_time(at, now)) }
				} @else if card.host.is_none() {
					span class="ui-res-when ui-res-none" { "—" }
				}
			}
			div class="ui-res-status" { (status_badge(card.status, flip)) }
			(metrics(card))
		}
	}
}

/// What a row looks like while the list is being fetched again: the same columns, so the
/// page does not jump when the rows arrive.
fn skeleton_rows() -> Markup {
	html! {
		div class="ui-res-skel" data-show="$_filtering" style="display: none" aria-hidden="true" {
			@for _ in 0..4 { (skeleton_row()) }
		}
	}
}

/// The region that polling and the filter re-render: the list, or what stands in for it.
/// `seen` is what the page last showed (see [`seen_of`]); `None` on a first render.
fn grid(
	s: &AppState,
	ctx: &TeamCtx,
	view: &EnvironmentView,
	filter: &Filter,
	now: OffsetDateTime,
	seen: Option<&str>,
	badges: &Badges,
) -> Markup {
	let shown: Vec<&ResourceCard> = view.resources.iter().filter(|c| filter.keeps(c)).collect();
	let busy = shown.iter().any(|c| in_flight(c.status));
	let fragment = fragment_url(ctx.team.id, view.project.id, &view.environment.slug);
	let create = create_url(ctx.team.id, view.project.id, &view.environment.slug);
	html! {
		div id="resource-grid" class="ui-res-region" {
			(poll(&fragment, busy))
			@if view.resources.is_empty() {
				(empty_state(
					"boxes",
					&format!("Nothing in {} yet", view.environment.name),
					Some("Create an application, a database or a model. Each one shows up here with its status."),
					picker::can_create_any(ctx).then(|| picker::open_button(&create)).as_ref(),
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
				p class="ui-res-count" aria-live="polite" {
					(shown.len()) @if shown.len() == 1 { " resource" } @else { " resources" }
					@if filter.is_active() { " match" }
				}
				(skeleton_rows())
				ul class="ui-res-list" "data-show"="!$_filtering" aria-label="Resources" {
					@for (i, card) in shown.iter().enumerate() {
						(row(s, ctx, view.project.id, card, now, i, changed_since(seen, card), badges))
					}
					@if picker::can_create_any(ctx) {
						li class="ui-res ui-res-new" style=(format!("--i:{}", shown.len())) {
							a href=(create) class="ui-res-newlink" {
								(icon("plus"))
								span class="ui-res-newt" { "Create resource" }
								span class="ui-res-news" { "Git repo · image · database · compose · template · model" }
							}
						}
					}
				}
			}
		}
	}
}

fn fragment_url(team: TeamId, project: ProjectId, slug: &str) -> String {
	format!("/teams/{team}/projects/{project}/{slug}/resources")
}

fn create_url(team: TeamId, project: ProjectId, slug: &str) -> String {
	format!("{}/create", env_url(team, project, slug))
}

fn filter_bar(view: &EnvironmentView, ctx: &TeamCtx, filter: &Filter) -> Markup {
	let page = env_url(ctx.team.id, view.project.id, &view.environment.slug);
	let fragment = fragment_url(ctx.team.id, view.project.id, &view.environment.slug);
	let get = format!("@get({})", js_string(&fragment));
	let kind = filter.kind.map_or("all", ResourceKind::slug);
	html! {
		form method="get" action=(page) role="search" class="ui-res-filter"
			"data-on:submit"=(format!("evt.preventDefault(); {get}")) {
			div class="ui-res-search" {
				span class="ui-res-search-ic" { (icon("search")) }
				input type="search" name="q" value=(filter.q) class=(INPUT) autocomplete="off"
					aria-label="Search resources" placeholder="Search resources"
					"data-bind:q" "data-indicator:_filtering" "data-on:input__debounce.250ms"=(get);
			}
			select name="type" class={ (SELECT) " ui-res-type" } aria-label="Filter by type"
				"data-bind:type" "data-indicator:_filtering" "data-on:change"=(get) {
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
		div class="ui-res-tabrow" {
			div class="min-w-0 flex-1" { (tabs("Environments", &items)) }
			@if can_create {
				(dialog_open_button(
					"environment-create",
					&button_class(Variant::Ghost, Size::Icon),
					&html! { (icon("plus")) span class="sr-only" { "New environment" } },
				))
			}
			(dropdown_menu(
				"environment-menu",
				&button_class(Variant::Ghost, Size::Icon),
				&format!("Actions for the {} environment", view.environment.name),
				&icon("ellipsis"),
				&html! {
					@if can_update || can_delete {
						(menu_label("Environment"))
					}
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
					(menu_label("Project"))
					a href=(format!("{}/iac", project_url(ctx.team.id, view.project.id))) class=(MENU_ITEM) {
						(icon("file-text")) "Infrastructure as code"
					}
				},
			))
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
	let badges = badges_of(s, ctx, &view).await;
	let can_pick = picker::can_create_any(ctx);
	let can_create_env = ctx.perms.can(Action::Create, Resource::Project);
	let can_update = ctx.perms.can(Action::Update, Resource::Project);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Project);
	// An environment's private network, one per server it has resources on.
	let env_networks = crate::networks::service::for_environment(
		s,
		ctx.user.id,
		ctx.team.id,
		view.project.id,
		view.environment.id,
	)
	.await
	.unwrap_or_default();
	let signals = json!({
		"q": filter.q,
		"type": filter.kind.map_or("all", ResourceKind::slug),
		"newName": "",
		"newSlug": "",
		"envName": view.environment.name,
		"envId": view.environment.id.to_string(),
		"_error": "",
		"_filtering": false,
		"seen": seen_of(&view.resources),
	});

	let body = html! {
		div class="ui-wide ui-res-page" data-signals=(signals.to_string()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Project" }
					h1 { (view.project.name) }
					p class="ui-lede" { (summary_line(&view)) }
					(network_line(ctx.team.id, &env_networks))
				}
				div class="ui-page-actions" {
					a class=(button_class(Variant::Ghost, Size::Md)) href={ (project_url(ctx.team.id, view.project.id)) "/canvas" } {
						(icon("network")) span { "Canvas" }
					}
					@if ctx.perms.can(Action::Read, Resource::Cron) {
						a class=(button_class(Variant::Ghost, Size::Md)) href={ (project_url(ctx.team.id, view.project.id)) "/cron" } {
							(icon("clock")) span { "Scheduled tasks" }
						}
					}
					@if can_pick {
						(picker::open_button(&create_url(ctx.team.id, view.project.id, &view.environment.slug)))
					}
				}
			}
			(tab_row(ctx, &view))
			(filter_bar(&view, ctx, filter))
			(grid(s, ctx, &view, filter, now, None, &badges))
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
			// The page tells what it last showed; the rows whose status moved play the
			// change animation, then `seen` catches up so the next poll does not repeat it.
			let seen = (!sig.seen.is_empty()).then_some(sig.seen.as_str());
			let badges = badges_of(&s, &ctx, &view).await;
			let markup = grid(
				&s,
				&ctx,
				&view,
				&filter,
				OffsetDateTime::now_utc(),
				seen,
				&badges,
			);
			sse::patch(&markup)
				.and(sse::signals(&json!({ "seen": seen_of(&view.resources) })))
				.into_response()
		}
		// A real 404 (not an SSE message): a poll that lost its environment stops.
		Err(e) => error::page(&ctx, &e),
	}
}

/// The create-resource picker: a page of its own, so it has room for what each choice
/// involves. Every link on it carries `?environment=<id>` into the create flows.
async fn create_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, slug)): Path<(TeamId, ProjectId, String)>,
) -> Result<Markup, Response> {
	let view =
		overview::environment_view(&s.db, ctx.user.id, ctx.team.id, project, Pick::Slug(&slug))
			.await
			.map_err(|e| error::page(&ctx, &e))?;
	if !picker::can_create_any(&ctx) {
		return Err(error::page(&ctx, &crate::error::ServerError::Forbidden));
	}
	let env_href = env_url(ctx.team.id, view.project.id, &view.environment.slug);
	let crumbs: Vec<Crumb<'_>> = vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(view.project.name.as_str(), Some(env_href)),
		("Create resource", None),
	];
	let body = picker::page(&ctx, &view.project, &view.environment);
	Ok(app_page(&ctx, "Create resource", &crumbs, &body))
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
