//! The strip above the page: sidebar toggle (a drawer button on small screens), breadcrumbs
//! with a switcher on the team, project and environment, and the search button.
//!
//! Breadcrumbs follow the hierarchy: team › project › environment › resource. The caller
//! passes the part from the section down (`Projects › api › Settings`); the shell puts the
//! team in front and, on a project's pages, adds the environment after the project and
//! turns each of those into a switcher. On a narrow screen the middle of a long trail
//! folds into a "…" menu, keeping the first and the last two.

use maud::{Markup, html};

use super::prefs::sidebar_collapsed;
use super::sidebar::team_items;
use crate::ui::components::{
	MENU_ITEM, Size, Variant, button_class, dialog_open_button, dropdown_menu, icon, kbd,
	menu_label, menu_link, menu_separator,
};
use crate::ui::extract::{ProjectScope, TeamCtx};
use crate::ui::layout::Crumb;

const SEARCH_BUTTON: &str = "ui-search-button inline-flex h-9 cursor-pointer items-center gap-2 rounded-md border border-input bg-background px-3 text-sm text-muted-foreground shadow-xs outline-hidden transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring/50";
const SWITCH_TRIGGER: &str = "ui-crumb-switch";

/// One entry of a switcher.
struct Choice {
	label: String,
	href: String,
	current: bool,
}

/// What opens beside a segment: a heading, the choices, and a link to the whole list.
struct Switcher {
	id: &'static str,
	/// The accessible name of the button ("Switch project").
	label: &'static str,
	heading: &'static str,
	choices: Vec<Choice>,
	all: Option<(&'static str, String)>,
}

struct Segment {
	label: String,
	/// `None` for the page the user is on.
	href: Option<String>,
	switcher: Option<Switcher>,
}

fn project_url(ctx: &TeamCtx, project: i64) -> String {
	format!("/teams/{}/projects/{project}", ctx.team.id)
}

fn projects_url(ctx: &TeamCtx) -> String {
	format!("/teams/{}/projects", ctx.team.id)
}

/// The full trail for a page: team, then the caller's crumbs with the project's
/// environment spliced in.
fn segments(ctx: &TeamCtx, crumbs: &[Crumb<'_>]) -> Vec<Segment> {
	let mut out = vec![Segment {
		label: ctx.team.name.clone(),
		href: Some(projects_url(ctx)),
		switcher: None,
	}];
	let scope = ctx.scope.as_ref();
	let list = projects_url(ctx);
	for (i, (label, href)) in crumbs.iter().enumerate() {
		let last = i + 1 == crumbs.len();
		// "Projects" is the sidebar's job once the project itself is in the trail.
		if scope.is_some() && href.as_deref() == Some(list.as_str()) && !last {
			continue;
		}
		if let Some(scope) =
			scope.filter(|s| is_project_crumb(ctx, s, label, href.as_deref(), last))
		{
			out.extend(project_segments(ctx, scope, last));
			continue;
		}
		out.push(Segment {
			label: (*label).to_owned(),
			href: href.clone(),
			switcher: None,
		});
	}
	out
}

fn is_project_crumb(
	ctx: &TeamCtx,
	scope: &ProjectScope,
	label: &str,
	href: Option<&str>,
	last: bool,
) -> bool {
	let url = project_url(ctx, scope.project.id.0);
	href == Some(url.as_str()) || (last && href.is_none() && label == scope.project.name)
}

/// The project, then its environment. On the project's own page the environment is the
/// page; elsewhere both are links back.
fn project_segments(ctx: &TeamCtx, scope: &ProjectScope, on_project_page: bool) -> [Segment; 2] {
	let url = project_url(ctx, scope.project.id.0);
	let project = Segment {
		label: scope.project.name.clone(),
		href: Some(url.clone()),
		switcher: Some(Switcher {
			id: "crumb-project",
			label: "Switch project",
			heading: "Projects",
			choices: scope
				.projects
				.iter()
				.map(|p| Choice {
					label: p.name.clone(),
					href: project_url(ctx, p.id.0),
					current: p.id == scope.project.id,
				})
				.collect(),
			all: Some(("All projects", projects_url(ctx))),
		}),
	};
	let env = Segment {
		label: scope.environment.name.clone(),
		href: (!on_project_page).then(|| format!("{url}?environment={}", scope.environment.id)),
		switcher: Some(Switcher {
			id: "crumb-environment",
			label: "Switch environment",
			heading: "Environments",
			choices: scope
				.environments
				.iter()
				.map(|e| Choice {
					label: e.name.clone(),
					href: format!("{url}?environment={}", e.id),
					current: e.id == scope.environment.id,
				})
				.collect(),
			all: None,
		}),
	};
	[project, env]
}

fn switcher_menu(sw: &Switcher, segment: &str) -> Markup {
	let items = html! {
		(menu_label(sw.heading))
		@for c in &sw.choices {
			a href=(c.href) class=(MENU_ITEM) aria-current=[c.current.then_some("true")] {
				span class="min-w-0 flex-1 truncate" { (c.label) }
				@if c.current { (icon("check")) }
			}
		}
		@if let Some((label, href)) = &sw.all {
			(menu_separator())
			(menu_link(href, &html! { (label) }))
		}
	};
	dropdown_menu(
		sw.id,
		SWITCH_TRIGGER,
		&format!("{}: {segment}", sw.label),
		&html! { (icon("chevrons-up-down")) },
		&items,
	)
}

fn crumb_inner(seg: &Segment, team_menu: Option<Markup>) -> Markup {
	html! {
		@if let Some(href) = &seg.href {
			a href=(href) class="ui-crumb-link" { (seg.label) }
		} @else {
			span class="ui-crumb-current" aria-current="page" { (seg.label) }
		}
		@if let Some(sw) = &seg.switcher {
			(switcher_menu(sw, &seg.label))
		}
		@if let Some(menu) = team_menu { (menu) }
	}
}

fn team_switcher(ctx: &TeamCtx) -> Markup {
	dropdown_menu(
		"crumb-team",
		SWITCH_TRIGGER,
		&format!("Switch team: {}", ctx.team.name),
		&html! { (icon("chevrons-up-down")) },
		&team_items(ctx),
	)
}

fn breadcrumbs(ctx: &TeamCtx, crumbs: &[Crumb<'_>]) -> Markup {
	let segs = segments(ctx, crumbs);
	let n = segs.len();
	// Middle segments fold away on narrow screens: not the first, not the last two.
	let folded = |i: usize| i != 0 && i + 2 < n;
	let hidden: Vec<&Segment> = segs
		.iter()
		.enumerate()
		.filter(|(i, s)| folded(*i) && s.href.is_some())
		.map(|(_, s)| s)
		.collect();
	html! {
		nav aria-label="Breadcrumb" class="ui-crumbs min-w-0 flex-1" {
			ol class="flex min-w-0 items-center text-sm" {
				@for (i, seg) in segs.iter().enumerate() {
					@let inner = crumb_inner(seg, (i == 0).then(|| team_switcher(ctx)));
					@if folded(i) {
						li class="ui-crumb ui-crumb-mid" { (inner) }
					} @else {
						li class="ui-crumb" { (inner) }
					}
					@if i == 0 && !hidden.is_empty() {
						li class="ui-crumb ui-crumb-more" {
							(dropdown_menu(
								"crumb-more",
								SWITCH_TRIGGER,
								"Show the rest of the path",
								&html! { (icon("ellipsis")) },
								&html! {
									@for seg in &hidden {
										@if let Some(href) = &seg.href { (menu_link(href, &html! { (seg.label) })) }
									}
								},
							))
						}
					}
				}
			}
		}
	}
}

/// The header bar. `crumbs` run from the section down to the current page.
pub fn bar(ctx: &TeamCtx, crumbs: &[Crumb<'_>]) -> Markup {
	let ghost = button_class(Variant::Ghost, Size::Icon);
	html! {
		header class="ui-header ui-vt-header" {
			span class="md:hidden" {
				(dialog_open_button("nav-drawer", &ghost, &html! {
					(icon("menu"))
					span class="ui-visually-hidden" { "Open menu" }
				}))
			}
			span class="hidden md:inline-flex" {
				button type="button" class=(ghost)
					"data-on:click"="$_collapsed = !$_collapsed; document.cookie = 'sidebar=' + ($_collapsed ? 'collapsed' : 'open') + '; path=/; max-age=31536000; samesite=lax'"
					"data-attr:aria-pressed"="$_collapsed" aria-pressed=(if sidebar_collapsed() { "true" } else { "false" }) {
					(icon("panel-left"))
					span class="ui-visually-hidden" { "Collapse the sidebar" }
				}
			}
			(breadcrumbs(ctx, crumbs))
			(dialog_open_button("palette", SEARCH_BUTTON, &html! {
				(icon("search"))
				span class="hidden sm:inline" { "Search" }
				span class="hidden items-center gap-1 sm:inline-flex" aria-hidden="true" {
					(kbd("Ctrl"))
					(kbd("K"))
				}
				span class="ui-visually-hidden sm:hidden" { "Search" }
			}))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn seg(label: &str, href: Option<&str>) -> Segment {
		Segment {
			label: label.to_owned(),
			href: href.map(str::to_owned),
			switcher: None,
		}
	}

	#[test]
	fn a_segment_with_a_page_is_a_link_and_the_last_is_current() {
		let html = crumb_inner(&seg("api <x>", Some("/p/1")), None).into_string();
		assert!(html.contains("<a href=\"/p/1\""), "{html}");
		assert!(html.contains("api &lt;x&gt;"), "{html}");
		let html = crumb_inner(&seg("Settings", None), None).into_string();
		assert!(html.contains("aria-current=\"page\""), "{html}");
	}
}
