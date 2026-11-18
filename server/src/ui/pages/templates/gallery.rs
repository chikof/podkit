//! The gallery: a grid of logo cards, searched on the server (debounced), grouped by a
//! category list, paged. A Browse/Explore surface: the grid is the page.
//!
//! Performance with the whole catalog (480 templates): the server sends one page of 48
//! cards at a time; cards below the first screen use `content-visibility: auto` with a
//! reserved size (so scrolling never shifts the layout) and lazy-load their logos. The grid
//! is keyboard-navigable: Tab reaches each card's link as usual and the arrow keys move
//! between cards by their position (`podkit.js`, `data-grid-nav`).

use axum::extract::{Path, Query, State};
use axum::response::Response;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::Place;
use crate::ids::{ProjectId, TeamId};
use crate::projects::{self, Project};
use crate::state::AppState;
use crate::templates::{self, CATEGORIES, Loaded};
use crate::ui::components::{
	INPUT, LogoSize, Priority, Size, Tone, Variant, badge, button_class, empty_state, icon,
	image_logo, link_button, link_card,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::sse::{self, js_string};

/// Cards per page.
const PAGE_SIZE: usize = 48;
/// Cards that may be in the first screen; later ones are deferred.
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

fn card(place: Place, l: &Loaded, index: usize) -> Markup {
	let t = &l.template;
	let priority = if index < FIRST_SCREEN {
		Priority::Normal
	} else {
		Priority::Low
	};
	let logo = image_logo(&crate::logos::template_url(l), LogoSize::Md, priority);
	let meta = html! {
		(badge(Tone::Neutral, category_label(&t.category)))
		(badge(Tone::Neutral, if l.plain_app { "App" } else { "Stack" }))
		@if !t.tags.is_empty() {
			span class="truncate text-xs text-muted-foreground" {
				@for (i, tag) in t.tags.iter().take(2).enumerate() {
					@if i > 0 { " · " }
					(tag)
				}
			}
		}
	};
	link_card(
		&detail_href(place, &t.id),
		&logo,
		&t.name,
		Some(&t.description),
		Some(&meta),
	)
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
	// AI and Databases first: they are what most people come here for.
	let mut order: Vec<&'static str> = vec!["ai", "database"];
	for c in CATEGORIES {
		if !order.contains(c) {
			order.push(c);
		}
	}
	html! {
		nav aria-label="Categories" class="ui-tpl-cats" {
			@let current = view.category.is_none();
			a href=(href(place, &all_view, 1)) class="ui-tpl-cat" aria-current=[current.then_some("page")] {
				span { "All" }
				span class="text-xs text-muted-foreground" { (total) }
			}
			@for category in order {
				@let count = counts.iter().find(|(c, _)| *c == category).map_or(0, |(_, n)| *n);
				@let current = view.category == Some(category);
				@if count > 0 || current {
					a href=(href(place, &View { category: Some(category), ..view.clone() }, 1))
						class="ui-tpl-cat" aria-current=[current.then_some("page")] {
						span { (category_label(category)) }
						span class="text-xs text-muted-foreground" { (count) }
					}
				}
			}
		}
	}
}

fn pager(place: Place, view: &View, page: usize, pages: usize) -> Markup {
	html! {
		@if pages > 1 {
			nav aria-label="Pages" class="flex items-center justify-between gap-3" {
				@if page > 1 {
					(link_button(Variant::Outline, Size::Md, &href(place, view, page - 1), &html! { (icon("chevron-left")) "Previous" }))
				} @else {
					span {}
				}
				span class="text-sm text-muted-foreground" { "Page " (page) " of " (pages) }
				@if page < pages {
					(link_button(Variant::Outline, Size::Md, &href(place, view, page + 1), &html! { "Next" (icon("chevron-right")) }))
				} @else {
					span {}
				}
			}
		}
	}
}

/// The region that search and paging re-render: the category list and the results.
pub(super) fn browser(place: Place, view: &View) -> Markup {
	let q = (!view.q.is_empty()).then_some(view.q.as_str());
	let by_text = templates::search(q, None);
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
	let hits: Vec<&Loaded> = by_text
		.iter()
		.copied()
		.filter(|l| view.category.is_none_or(|c| l.template.category == c))
		.collect();
	let pages = hits.len().div_ceil(PAGE_SIZE).max(1);
	let page = view.page.min(pages);
	let shown = hits.iter().skip((page - 1) * PAGE_SIZE).take(PAGE_SIZE);
	let filtered = q.is_some() || view.category.is_some();
	let clear = View::default();
	html! {
		div id="tpl-browser" class="ui-tpl-layout" {
			(category_nav(place, view, &counts, by_text.len()))
			div class="flex min-w-0 flex-col gap-4" {
				p class="text-sm text-muted-foreground" aria-live="polite" {
					(hits.len()) @if hits.len() == 1 { " template" } @else { " templates" }
					@if filtered { " match" }
					@if let Some(c) = view.category { " in " strong { (category_label(c)) } }
				}
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
					div class="ui-card-grid ui-tpl-grid" data-grid-nav {
						@for (i, l) in shown.enumerate() {
							(card(place, l, i))
						}
					}
					(pager(place, view, page, pages))
				}
			}
		}
	}
}

fn search_form(place: Place, view: &View) -> Markup {
	let fragment = format!("{}/results", place.base());
	let get = format!("@get({})", js_string(&fragment));
	html! {
		form method="get" action=(place.base()) role="search" class="flex flex-wrap items-center gap-2"
			"data-on:submit"=(format!("evt.preventDefault(); {get}")) {
			div class="relative min-w-48 max-w-xl flex-1" {
				span class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground" { (icon("search")) }
				input type="search" name="q" value=(view.q) class={ (INPUT) " pl-9" } autocomplete="off" maxlength=(MAX_QUERY)
					aria-label="Search templates" placeholder="Search templates"
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

fn render(ctx: &TeamCtx, place: Place, project: Option<&Project>, view: &View) -> Markup {
	let signals = json!({ "q": view.q, "category": view.category.unwrap_or_default() });
	let body = html! {
		div class="ui-wide flex w-full flex-col gap-6" data-signals=(signals.to_string()) {
			div class="flex min-w-0 flex-col gap-1" {
				h1 class="text-2xl font-semibold tracking-tight" { "Templates" }
				p class="text-sm text-muted-foreground" {
					@if let Some(p) = project {
						"Ready-made services to deploy into " strong { (p.name) } "."
					} @else {
						"Ready-made services. Pick one, answer a few questions, and it runs on your server."
					}
				}
			}
			(search_form(place, view))
			(browser(place, view))
		}
	};
	app_page(ctx, "Templates", &crumbs(ctx, project), &body)
}

pub(super) async fn page(
	ctx: TeamCtx,
	Path(_): Path<TeamId>,
	Query(q): Query<PageQuery>,
) -> Markup {
	let place = Place {
		team: ctx.team.id,
		project: None,
		environment: q.environment,
	};
	render(&ctx, place, None, &View::new(&q.q, &q.category, q.page))
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
	axum::response::IntoResponse::into_response(sse::patch(&browser(
		place,
		&View::new(&sig.q, &sig.category, None),
	)))
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
	axum::response::IntoResponse::into_response(sse::patch(&browser(
		place,
		&View::new(&sig.q, &sig.category, None),
	)))
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
}
