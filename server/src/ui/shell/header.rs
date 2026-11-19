//! The strip above the page: sidebar toggle (a drawer button on small screens), breadcrumbs
//! with a switcher on the team, project and environment, and the search button.
//!
//! Breadcrumbs follow the hierarchy: team › project › environment › resource. The caller
//! passes the part from the section down (`Projects › api › Settings`); the shell puts the
//! team in front and, on a project's pages, adds the environment after the project and
//! turns each of those into a switcher. On a narrow screen the middle of a long trail
//! folds into a "…" menu, keeping the last three.

use maud::{Markup, html};

use super::sidebar::team_items;
use crate::ui::components::{
	MENU_ITEM, dialog_open_button, dropdown_menu, icon, kbd, menu_label, menu_link, menu_separator,
};
use crate::ui::extract::{ProjectScope, TeamCtx};
use crate::ui::layout::Crumb;

const SEARCH_BUTTON: &str = "ui-search";
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
		let is_last = i + 1 == crumbs.len();
		// "Projects" is the sidebar's job once the project itself is in the trail.
		if scope.is_some() && href.as_deref() == Some(list.as_str()) && !is_last {
			continue;
		}
		if let Some(scope) =
			scope.filter(|s| is_project_crumb(ctx, s, label, href.as_deref(), is_last))
		{
			out.extend(project_segments(ctx, scope, is_last));
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
		href: (!on_project_page).then(|| format!("{url}/{}", scope.environment.slug)),
		switcher: Some(Switcher {
			id: "crumb-environment",
			label: "Switch environment",
			heading: "Environments",
			choices: scope
				.environments
				.iter()
				.map(|e| Choice {
					label: e.name.clone(),
					href: format!("{url}/{}", e.slug),
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
	// On a narrow screen everything but the last three segments folds into a "…" menu.
	let folded = |i: usize| i + 3 < n;
	let hidden: Vec<&Segment> = segs
		.iter()
		.enumerate()
		.filter(|(i, s)| folded(*i) && s.href.is_some())
		.map(|(_, s)| s)
		.collect();
	html! {
		nav aria-label="Breadcrumb" class="ui-crumbs min-w-0 flex-1" {
			ol class="flex min-w-0 items-center text-sm" {
				@if !hidden.is_empty() {
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
				@for (i, seg) in segs.iter().enumerate() {
					@let inner = crumb_inner(seg, (i == 0).then(|| team_switcher(ctx)));
					@if folded(i) {
						li class="ui-crumb ui-crumb-mid" { (inner) }
					} @else {
						li class="ui-crumb" { (inner) }
					}
				}
			}
		}
	}
}

/// The header bar. `crumbs` run from the section down to the current page.
pub fn bar(ctx: &TeamCtx, crumbs: &[Crumb<'_>]) -> Markup {
	html! {
		header class="ui-bar ui-vt-header" {
			(breadcrumbs(ctx, crumbs))
			div class="ui-bar-actions" {
				(dialog_open_button("palette", SEARCH_BUTTON, &html! {
					(icon("search"))
					span class="ui-search-label" { "Search or jump to…" }
					span class="ui-search-kbd" aria-hidden="true" { (kbd("Ctrl K")) }
				}))
				(theme_flip())
			}
		}
	}
}

/// One button that turns the theme to the other one. A form posting to `/theme`, so it works
/// without script. Both buttons are in the page and CSS shows the one that applies: the sun
/// (go light) while the page is dark, the moon (go dark) while it is light, whether the page
/// follows the system or the user chose.
pub fn theme_flip() -> Markup {
	html! {
		form method="post" action="/theme" class="contents" {
			button type="submit" name="theme" value="light" class="ui-icon-btn ui-only-dark" aria-label="Switch to the light theme" {
				(icon("sun"))
			}
			button type="submit" name="theme" value="dark" class="ui-icon-btn ui-only-light" aria-label="Switch to the dark theme" {
				(icon("moon"))
			}
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
