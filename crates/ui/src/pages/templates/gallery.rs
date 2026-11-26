//! The gallery: the catalog as rows, searched on the server (debounced), grouped by a row of
//! category chips, paged. An Explore surface: the list is the page.
//!
//! Performance with the whole catalog (480 templates): the server sends one page of 24 rows
//! at a time, each row as little markup as can say who it is (logo, name, one line, category,
//! version); rows below the first screen use `content-visibility: auto` with a reserved size
//! (so scrolling never shifts the layout) and lazy-load their logos. The list is
//! keyboard-navigable: Tab reaches each row's link as usual and the arrow keys move between
//! rows by their position (`podkit.js`, `data-grid-nav`).

use axum::extract::{Path, Query, State};
use axum::response::Response;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::Place;
use crate::components::{
	INPUT, LogoSize, Priority, Size, Variant, button_class, empty_state, icon, image_logo,
	link_button,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page};
use crate::sse::{self, js_string};
use podkit_core::ids::{ProjectId, TeamId};
use podkit_core::projects::{self, Project};
use podkit_core::quotas::service::is_instance_admin;
use podkit_core::state::AppState;
use podkit_core::templates::{self, CATEGORIES, Loaded};

/// Rows per page: two columns of twelve.
const PAGE_SIZE: usize = 24;
/// Rows that may be in the first screen; later ones are deferred.
pub(super) const FIRST_SCREEN: usize = 12;
/// Longest search text read.
const MAX_QUERY: usize = 100;

/// What the gallery shows: the search text, one category, one page.
#[derive(Debug, Clone, Default)]
pub(super) struct View {
	pub q: String,
	pub category: Option<&'static str>,
	pub page: usize,
}

impl View {
	pub fn new(q: &str, category: &str, page: Option<usize>) -> Self {
		Self {
			q: q.trim().chars().take(MAX_QUERY).collect(),
			// Only a known category counts, and the value kept is ours, not the request's.
			category: CATEGORIES.iter().copied().find(|c| *c == category),
			page: page.unwrap_or(1).max(1),
		}
	}
}

/// How a category reads in the list.
pub(super) fn category_label(category: &str) -> &'static str {
	match category {
		"ai" => "AI",
		"analytics" => "Analytics",
		"automation" => "Automation",
		"backup" => "Backup",
		"cms" => "CMS",
		"communication" => "Communication",
		"database" => "Databases",
		"development" => "Development",
		"finance" => "Finance",
		"games" => "Games",
		"media" => "Media",
		"monitoring" => "Monitoring",
		"networking" => "Networking",
		"productivity" => "Productivity",
		"security" => "Security",
		"storage" => "Storage",
		"tools" => "Tools",
		_ => "Other",
	}
}

/// A link to the gallery with this view, keeping the environment the picker chose.
fn href(place: Place, view: &View, page: usize) -> String {
	let mut query = url::form_urlencoded::Serializer::new(String::new());
	if !view.q.is_empty() {
		query.append_pair("q", &view.q);
	}
	if let Some(category) = view.category {
		query.append_pair("category", category);
	}
	if page > 1 {
		query.append_pair("page", &page.to_string());
	}
	if let Some(environment) = place.environment {
		query.append_pair("environment", &environment.to_string());
	}
	let query = query.finish();
	let base = place.base();
	if query.is_empty() {
		base
	} else {
		format!("{base}?{query}")
	}
}

/// The detail page of a template, keeping the project and environment.
pub(super) fn detail_href(place: Place, id: &str) -> String {
	let base = place.base();
	match place.env_param() {
		Some(env) => format!("{base}/{id}?{env}"),
		None => format!("{base}/{id}"),
	}
}

/// `v2.9.1`; a version that is a word (`latest`) is left as it is.
fn version_label(version: &str) -> String {
	if version.starts_with(|c: char| c.is_ascii_digit()) {
		format!("v{version}")
	} else {
		version.to_owned()
	}
}

/// One template as one row: the whole row is the link (the name's `::after` covers it).
fn row(place: Place, l: &Loaded, index: usize) -> Markup {
	let t = &l.template;
	let priority = if index < FIRST_SCREEN {
		Priority::Normal
	} else {
		Priority::Low
	};
	html! {
		li class="ui-link-card ui-tpl-row" {
			(image_logo(&podkit_core::logos::template_url(l), LogoSize::Md, priority))
			div class="ui-tpl-main" {
				h2 { a href=(detail_href(place, &t.id)) class="ui-link-card-link" { (t.name) } }
				p { (t.description) }
			}
			div class="ui-tpl-meta" {
				span { (category_label(&t.category)) }
				@if !t.version.is_empty() { span class="ui-tpl-ver" { (version_label(&t.version)) } }
			}
		}
	}
}

fn category_nav(
	place: Place,
	view: &View,
	counts: &[(&'static str, usize)],
	total: usize,
) -> Markup {
	let all_view = View {
		category: None,
		..view.clone()
	};
	html! {
		nav aria-label="Categories" class="ui-tpl-cats" {
			@let current = view.category.is_none();
			a href=(href(place, &all_view, 1)) class="ui-chip" aria-current=[current.then_some("page")] {
				span { "All" }
				span class="ui-chip-n" { (total) }
			}
			@for (category, count) in counts {
				@let current = view.category == Some(*category);
				@if *count > 0 || current {
					a href=(href(place, &View { category: Some(category), ..view.clone() }, 1))
						class="ui-chip" aria-current=[current.then_some("page")] {
						span { (category_label(category)) }
						span class="ui-chip-n" { (count) }
					}
				}
			}
		}
	}
}

fn pager(place: Place, view: &View, page: usize, pages: usize) -> Markup {
	html! {
		@if pages > 1 {
			nav aria-label="Pages" class="ui-tpl-pager" {
				@if page > 1 {
					(link_button(Variant::Outline, Size::Md, &href(place, view, page - 1), &html! { (icon("chevron-left")) "Previous" }))
				} @else {
					span {}
				}
				span class="ui-hint" { "Page " (page) " of " (pages) }
				@if page < pages {
					(link_button(Variant::Outline, Size::Md, &href(place, view, page + 1), &html! { "Next" (icon("chevron-right")) }))
				} @else {
					span {}
				}
			}
		}
	}
}

/// The templates that match the search text, whatever their category.
fn by_text(view: &View) -> Vec<std::sync::Arc<Loaded>> {
	templates::search((!view.q.is_empty()).then_some(view.q.as_str()), None)
}

fn in_category<'a>(view: &View, by_text: &'a [std::sync::Arc<Loaded>]) -> Vec<&'a Loaded> {
	by_text
		.iter()
		.map(|l| &**l)
		.filter(|l| view.category.is_none_or(|c| l.template.category == c))
		.collect()
}

fn page_of(view: &View, hits: usize) -> (usize, usize) {
	let pages = hits.div_ceil(PAGE_SIZE).max(1);
	(view.page.min(pages), pages)
}

/// The line under the title: how many are shown, and how many there are. It is its own
/// region because the title is not part of what search re-renders.
pub(super) fn count_line(view: &View) -> Markup {
	let total = templates::search(None, None).len();
	let hits = in_category(view, &by_text(view)).len();
	let (page, _) = page_of(view, hits);
	let shown = hits.saturating_sub((page - 1) * PAGE_SIZE).min(PAGE_SIZE);
	let filtered = !view.q.is_empty() || view.category.is_some();
	html! {
		p id="tpl-count" class="ui-lede" aria-live="polite" {
			(shown) @if shown == 1 { " template" } @else { " templates" } " shown"
			@if filtered { " \u{b7} " (hits) " match" }
			" \u{b7} " (total) " in the catalog"
			@if let Some(c) = view.category { " \u{b7} in " strong { (category_label(c)) } }
		}
	}
}

/// The region that search and paging re-render: the category chips and the results.
pub(super) fn browser(place: Place, view: &View) -> Markup {
	let by_text = by_text(view);
	let counts: Vec<(&'static str, usize)> = CATEGORIES
		.iter()
		.copied()
		.map(|c| {
			(
				c,
				by_text.iter().filter(|l| l.template.category == c).count(),
			)
		})
		.collect();
	let hits = in_category(view, &by_text);
	let (page, pages) = page_of(view, hits.len());
	let shown = hits.iter().skip((page - 1) * PAGE_SIZE).take(PAGE_SIZE);
	let clear = View::default();
	html! {
		div id="tpl-browser" class="ui-tpl-layout" {
			(category_nav(place, view, &counts, by_text.len()))
			@if hits.is_empty() {
				(empty_state(
					"search",
					"No template matches",
					Some("Try another word, or look in all categories."),
					Some(&html! {
						a class=(button_class(Variant::Outline, Size::Md)) href=(href(place, &clear, 1)) { "Clear the search" }
					}),
				))
			} @else {
				ul class="ui-tpl-list" data-grid-nav {
					@for (i, l) in shown.enumerate() {
						(row(place, l, i))
					}
				}
				(pager(place, view, page, pages))
			}
		}
	}
}

fn search_form(place: Place, view: &View) -> Markup {
	let fragment = format!("{}/results", place.base());
	let get = format!("@get({})", js_string(&fragment));
	let total = templates::search(None, None).len();
	html! {
		form method="get" action=(place.base()) role="search" class="ui-tpl-search"
			"data-on:submit"=(format!("evt.preventDefault(); {get}")) {
			div class="ui-field-icon" {
				(icon("search"))
				input type="search" name="q" value=(view.q) class=(INPUT) autocomplete="off" maxlength=(MAX_QUERY)
					aria-label="Search templates" placeholder={ "Search " (total) " templates: gotify, plausible, n8n\u{2026}" }
					"data-bind:q" "data-on:input__debounce.300ms"=(get);
			}
			@if let Some(category) = view.category {
				input type="hidden" name="category" value=(category);
			}
			@if let Some(env) = place.environment {
				input type="hidden" name="environment" value=(env);
			}
			noscript { button type="submit" class=(button_class(Variant::Outline, Size::Md)) { "Search" } }
		}
	}
}

fn crumbs<'a>(ctx: &TeamCtx, project: Option<&'a Project>) -> Vec<Crumb<'a>> {
	match project {
		Some(p) => vec![
			("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
			(
				p.name.as_str(),
				Some(format!("/teams/{}/projects/{}", ctx.team.id, p.id)),
			),
			("Templates", None),
		],
		None => vec![("Templates", None)],
	}
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct PageQuery {
	#[serde(default)]
	q: String,
	#[serde(default)]
	category: String,
	#[serde(default)]
	page: Option<usize>,
	#[serde(default)]
	environment: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct Signals {
	#[serde(default)]
	q: String,
	#[serde(default)]
	category: String,
}

/// The site the catalog is browsed on, for whoever cannot refresh it here.
const CATALOG_SITE: &str = "https://podoes.github.io/podkit-templates/";

/// Which catalog the gallery shows and, for the administrator, the button that fetches it again.
fn catalog_line(ctx: &TeamCtx, admin: bool) -> Markup {
	let registry = templates::load_all();
	let refresh = format!(
		"@post({})",
		js_string(&format!("/teams/{}/templates/refresh", ctx.team.id))
	);
	html! {
		p class="ui-lede" {
			@if !registry.commit.is_empty() {
				"Catalog " code { (registry.commit.chars().take(7).collect::<String>()) } ". "
			}
			@if admin {
				button type="button" class=(button_class(Variant::Outline, Size::Sm))
					"data-on:click"=(refresh) "data-indicator:refreshing" "data-attr:disabled"="$refreshing" {
					"Refresh the catalog"
				}
			}
		}
	}
}

/// The gallery before the first refresh: podkit ships no templates of its own.
fn no_catalog(ctx: &TeamCtx, admin: bool) -> Markup {
	let refresh = format!(
		"@post({})",
		js_string(&format!("/teams/{}/templates/refresh", ctx.team.id))
	);
	let action = html! {
		@if admin {
			button type="button" class=(button_class(Variant::Default, Size::Md))
				"data-on:click"=(refresh) "data-indicator:refreshing" "data-attr:disabled"="$refreshing" {
				"Download the catalog"
			}
		}
		a class=(button_class(Variant::Outline, Size::Md)) href=(CATALOG_SITE) rel="noopener" target="_blank" {
			"Browse the catalog online"
		}
	};
	let lede = if admin {
		"podkit downloads its templates once and then works without a connection. This needs internet access."
	} else {
		"The administrator of this podkit has not downloaded the template catalog yet."
	};
	empty_state("boxes", "No templates yet", Some(lede), Some(&action))
}

fn render(
	ctx: &TeamCtx,
	place: Place,
	project: Option<&Project>,
	view: &View,
	admin: bool,
) -> Markup {
	let empty = templates::load_all().templates.is_empty();
	let signals = json!({ "q": view.q, "category": view.category.unwrap_or_default() });
	let body = html! {
		div class="ui-tpl-page" data-signals=(signals.to_string()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "One-click apps" }
					h1 { "Templates" }
					@if !empty {
						(count_line(view))
						(catalog_line(ctx, admin))
					}
					@if let Some(p) = project {
						p class="ui-lede" { "Ready-made services to deploy into " strong { (p.name) } "." }
					}
				}
			}
			@if empty {
				(no_catalog(ctx, admin))
			} @else {
				(search_form(place, view))
				(browser(place, view))
			}
		}
	};
	app_page(ctx, "Templates", &crumbs(ctx, project), &body)
}

/// The answer to a search: the count line and the results region, patched.
fn results_patch(place: Place, view: &View) -> Response {
	axum::response::IntoResponse::into_response(
		sse::patch(&count_line(view)).and(sse::patch(&browser(place, view))),
	)
}

/// Whether the viewer may refresh the catalog, which belongs to the whole instance.
async fn is_admin(s: &AppState, ctx: &TeamCtx) -> bool {
	is_instance_admin(&s.db, ctx.user.id).await.unwrap_or(false)
}

/// `POST /teams/{t}/templates/refresh`: fetch the catalog again, then show it.
pub(super) async fn refresh(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path(_): Path<TeamId>,
) -> Response {
	let events = if is_admin(&s, &ctx).await {
		match templates::catalog::refresh(&s).await {
			Ok(_) => sse::reload(),
			Err(e) => sse::error(&e.to_string()),
		}
	} else {
		sse::error("Only the administrator of this podkit can refresh the catalog.")
	};
	axum::response::IntoResponse::into_response(events)
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path(_): Path<TeamId>,
	Query(q): Query<PageQuery>,
) -> Markup {
	let admin = is_admin(&s, &ctx).await;
	let place = Place {
		team: ctx.team.id,
		project: None,
		environment: q.environment,
	};
	render(
		&ctx,
		place,
		None,
		&View::new(&q.q, &q.category, q.page),
		admin,
	)
}

pub(super) async fn results(
	ctx: TeamCtx,
	Path(_): Path<TeamId>,
	ReadSignals(sig): ReadSignals<Option<Signals>>,
) -> Response {
	let sig = sig.unwrap_or_default();
	let place = Place {
		team: ctx.team.id,
		project: None,
		environment: None,
	};
	results_patch(place, &View::new(&sig.q, &sig.category, None))
}

pub(super) async fn project_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<PageQuery>,
) -> Result<Markup, Response> {
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let place = Place {
		team: ctx.team.id,
		project: Some(project.id),
		environment: q.environment,
	};
	Ok(render(
		&ctx,
		place,
		Some(&project),
		&View::new(&q.q, &q.category, q.page),
		is_admin(&s, &ctx).await,
	))
}

pub(super) async fn project_results(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Option<Signals>>,
) -> Response {
	let sig = sig.unwrap_or_default();
	let project = match projects::service::find(&s.db, ctx.user.id, ctx.team.id, project).await {
		Ok(p) => p,
		Err(e) => return error::page(&ctx, &e),
	};
	let place = Place {
		team: ctx.team.id,
		project: Some(project.id),
		environment: None,
	};
	results_patch(place, &View::new(&sig.q, &sig.category, None))
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn an_unknown_category_is_ignored_and_the_page_never_drops_below_one() {
		let v = View::new("  nginx  ", "<script>", Some(0));
		assert_eq!(v.q, "nginx");
		assert_eq!(v.category, None);
		assert_eq!(v.page, 1);
		assert_eq!(View::new("", "ai", Some(3)).category, Some("ai"));
	}

	#[test]
	fn links_escape_the_search_text_and_keep_the_environment() {
		let place = Place {
			team: TeamId(1),
			project: Some(ProjectId(2)),
			environment: Some(7),
		};
		let v = View::new("a&b=c <x>", "ai", None);
		let link = href(place, &v, 2);
		assert_eq!(
			link,
			"/teams/1/projects/2/templates?q=a%26b%3Dc+%3Cx%3E&category=ai&page=2&environment=7"
		);
	}

	#[test]
	fn the_first_page_of_everything_has_a_plain_link() {
		let place = Place {
			team: TeamId(1),
			project: None,
			environment: None,
		};
		assert_eq!(href(place, &View::default(), 1), "/teams/1/templates");
	}

	#[test]
	fn a_version_is_prefixed_only_when_it_is_a_number() {
		assert_eq!(version_label("2.9.1"), "v2.9.1");
		assert_eq!(version_label("latest"), "latest");
	}
}
