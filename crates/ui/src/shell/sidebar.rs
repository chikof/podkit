//! The left column (a brand strip on a small screen): the brand, the team switcher, the
//! navigation and the user menu; and the mobile tab bar, which lists the same places from
//! the same `nav` data.
//!
//! The two navigations are two landmarks, and CSS shows one of them: the sidebar's above
//! 760 px, the tab bar below. Each hidden one is `display: none`, so a screen reader never
//! meets the places twice.

use maud::{Markup, html};

use super::nav::{self, NavItem};
use crate::components::{
	MENU_ITEM, MarkSize, dropdown_menu, icon, kbd, menu_label, menu_link, menu_separator,
	team_mark, user_avatar,
};
use crate::extract::TeamCtx;
use crate::theme::{self, Theme};
use podkit_core::servers::AgentSummary;

const TEAM_TRIGGER: &str = "ui-team-trigger";
const USER_TRIGGER: &str = "ui-user-trigger";

/// The podkit mark: "pk" on a teal tile. Decoration: the name beside it is the text.
fn brand_mark() -> Markup {
	html! { span class="ui-brand-mark" aria-hidden="true" { "pk" } }
}

fn brand_in(href: &str) -> Markup {
	html! {
		a class="ui-brand" href=(href) {
			(brand_mark())
			span class="ui-brand-name" { "podkit" }
		}
	}
}

/// The brand: the mark and the name, as a link to `href`.
pub fn brand(href: &str) -> Markup {
	brand_in(href)
}

fn side_brand(ctx: &TeamCtx) -> Markup {
	brand_in(&format!("/teams/{}/projects", ctx.team.id))
}

/// One place. `tip` is the id of the rail tooltip: the sidebar's links carry one (it only
/// opens while the sidebar is folded to icons); the tab bar's do not.
fn link(item: &NavItem, current: bool, tip: Option<&str>) -> Markup {
	html! {
		li {
			a href=(item.href) class="ui-nav-link" aria-current=[current.then_some("page")] interestfor=[tip] {
				(icon(item.icon))
				span class="ui-nav-label" { (item.label) }
			}
			@if let Some(tip) = tip {
				div id=(tip) popover="hint" class="ui-tip ui-tip-side" { (item.label) }
			}
		}
	}
}

/// The team's name as a button that lists the user's teams. Under the name, the team's
/// slug, so two teams with one name still tell apart.
fn team_menu(ctx: &TeamCtx) -> Markup {
	let trigger = html! {
		(team_mark(&ctx.team, MarkSize::Medium))
		span class="ui-team-name" {
			span class="ui-team-title" { (ctx.team.name) }
			span class="ui-team-sub" { "Workspace · " (ctx.team.slug) }
		}
		span class="ui-team-chev" { (icon("chevrons-up-down")) }
	};
	dropdown_menu(
		"side-team-menu",
		TEAM_TRIGGER,
		&format!("Team: {}. Switch team", ctx.team.name),
		&trigger,
		&team_items(ctx),
	)
}

/// The list inside a team switcher, shared by the sidebar and the breadcrumb.
pub fn team_items(ctx: &TeamCtx) -> Markup {
	html! {
		(menu_label("Teams"))
		@for team in &ctx.teams {
			a href={ "/teams/" (team.id.0) "/projects" } class=(MENU_ITEM)
				aria-current=[(team.id == ctx.team.id).then_some("true")] {
				(team_mark(team, MarkSize::Small))
				span class="ui-menu-grow" { (team.name) }
				@if team.id == ctx.team.id { (icon("check")) }
			}
		}
		(menu_separator())
		(menu_link(&format!("/teams/{}/settings", ctx.team.id), &html! { (icon("settings")) "Team settings" }))
		(menu_link("/teams/new", &html! { (icon("plus")) "New team" }))
	}
}

/// The signed-in user: the avatar and name at the foot of the sidebar. The menu has the
/// theme and Sign out. On a small screen only the avatar stays.
fn user_menu(ctx: &TeamCtx) -> Markup {
	let current = theme::current();
	let trigger = html! {
		(user_avatar(&ctx.user.name, ctx.user.id, ctx.user.avatar.as_deref()))
		span class="ui-user-name" {
			span class="ui-user-title" { (ctx.user.name) }
			span class="ui-user-sub" { "Account and theme" }
		}
		span class="ui-team-chev" { (icon("chevrons-up-down")) }
	};
	let theme_item = |choice: Theme, label: &str, icon_name: &str| {
		html! {
			button type="submit" name="theme" value=(choice.as_str()) class=(MENU_ITEM) {
				(icon(icon_name))
				span class="ui-menu-grow" { (label) }
				@if choice == current { (icon("check")) }
			}
		}
	};
	let items = html! {
		(menu_label(&ctx.user.name))
		(menu_link(&format!("/teams/{}/account", ctx.team.id), &html! { (icon("user")) "Account" }))
		(menu_separator())
		(menu_label("Theme"))
		form method="post" action="/theme" class="contents" {
			(theme_item(Theme::System, "System", "monitor"))
			(theme_item(Theme::Light, "Light", "sun"))
			(theme_item(Theme::Dark, "Dark", "moon"))
		}
		(menu_separator())
		form method="post" action="/logout" class="contents" {
			button type="submit" class=(MENU_ITEM) {
				(icon("log-out"))
				span class="ui-menu-grow" { "Sign out" }
			}
		}
	};
	dropdown_menu(
		"side-user-menu",
		USER_TRIGGER,
		&format!("Account menu for {}", ctx.user.name),
		&trigger,
		&items,
	)
}

/// The fold toggle at the foot of the sidebar. `[` does the same (the shell's `_side` signal).
/// Its label and tooltip name the action for the state it is in.
fn collapse_toggle(collapsed: bool) -> Markup {
	let name = if collapsed {
		"Expand sidebar"
	} else {
		"Collapse sidebar"
	};
	html! {
		button type="button" class="ui-side-toggle" data-side-toggle aria-label=(name) aria-keyshortcuts="["
			interestfor="side-toggle-tip" "data-on:click"="$_side = !$_side"
			"data-attr:aria-label"="$_side ? 'Expand sidebar' : 'Collapse sidebar'" {
			(icon("panel-left"))
			span class="ui-nav-label" { "Collapse" }
		}
		div id="side-toggle-tip" popover="hint" class="ui-tip ui-tip-side" {
			span class="ui-when-open" { "Collapse sidebar" }
			span class="ui-when-collapsed" { "Expand sidebar" }
			" " (kbd("["))
		}
	}
}

/// The colour of the card's dot: every server answers, some do, none does, or there is none.
fn agent_dot(summary: &AgentSummary) -> &'static str {
	match (summary.connected, summary.total) {
		(_, 0) => "ui-dot ui-dot-neutral",
		(c, t) if c >= t => "ui-dot ui-dot-success",
		(0, _) => "ui-dot ui-dot-danger",
		_ => "ui-dot ui-dot-warning",
	}
}

/// "2 of 3 servers connected", or "1 server connected" when they all are.
fn agent_count(summary: &AgentSummary) -> String {
	let noun = if summary.total == 1 {
		"server"
	} else {
		"servers"
	};
	if summary.total == 0 {
		"No servers yet".to_owned()
	} else if summary.connected >= summary.total {
		format!("{} {noun} connected", summary.total)
	} else {
		format!(
			"{} of {} {noun} connected",
			summary.connected, summary.total
		)
	}
}

/// The agent card at the foot of the sidebar: the Podman the team's agents run and how
/// many servers answer, as a link to the servers page. Only for someone who may read
/// servers (`ctx.agents` is `None` otherwise). Folded to the rail it is the dot alone, with
/// the figures in its tooltip; on a small screen it is not shown.
fn agent_card(ctx: &TeamCtx) -> Markup {
	let Some(summary) = &ctx.agents else {
		return html! {};
	};
	let podman = match (summary.podman_label(), summary.mode()) {
		(Some(version), Some(mode)) => Some(format!("{version} · {mode}")),
		(Some(version), None) => Some(version),
		(None, Some(mode)) => Some(format!("Podman · {mode}")),
		(None, None) => None,
	};
	let count = agent_count(summary);
	html! {
		a class="ui-agent-card" href={ "/teams/" (ctx.team.id) "/servers" } interestfor="agent-card-tip" {
			span class=(agent_dot(summary)) aria-hidden="true" {}
			span class="ui-agent-text" {
				span class="ui-agent-title" { "podkit agent" }
				@if let Some(podman) = &podman {
					span class="ui-agent-line" { (podman) }
				}
				span class="ui-agent-line" { (count) }
			}
		}
		div id="agent-card-tip" popover="hint" class="ui-tip ui-tip-side" {
			@if let Some(podman) = &podman { (podman) " · " }
			(count)
		}
	}
}

/// The sidebar. Above 760 px it is the left column (or the icon rail, when the user folded
/// it); below, the strip across the top. The places come in headed groups: Build (the daily
/// places), Infrastructure (what the team runs on), Observe and Admin, which ends with
/// Settings. Every link carries a rail tooltip that only opens while the sidebar is folded
/// to icons.
pub fn aside(ctx: &TeamCtx) -> Markup {
	let groups = nav::sidebar_groups(ctx);
	let current = nav::sidebar_current(ctx);
	let collapsed = super::sidebar_collapsed();
	let is_current = |item: &NavItem| current.as_deref() == Some(item.href.as_str());
	let mut tip = 0..;
	html! {
		aside id="side" class="ui-sidebar ui-vt-chrome" aria-label="Application" {
			(side_brand(ctx))
			(team_menu(ctx))
			nav class="ui-nav" aria-label="Primary" {
				@for group in &groups {
					div class="ui-nav-group" {
						p class="ui-nav-heading" id=(group.id) { (group.label) }
						ul class="ui-nav-list" aria-labelledby=(group.id) {
							@for item in &group.items {
								@let n = tip.next().unwrap_or_default();
								(link(item, is_current(item), Some(&format!("nav-tip-{n}"))))
							}
						}
					}
				}
			}
			div class="ui-chrome-foot" {
				(collapse_toggle(collapsed))
				(agent_card(ctx))
				(user_menu(ctx))
			}
		}
	}
}

/// The bottom tab bar of a small screen: the same places, one tap each.
pub fn tab_bar(ctx: &TeamCtx) -> Markup {
	let items = nav::tab_items(ctx);
	let current = nav::primary_current(ctx);
	html! {
		nav class="ui-tabbar" aria-label="Tabs" {
			ul class="ui-nav-list" {
				@for item in &items {
					(link(item, current.as_deref() == Some(item.href.as_str()), None))
				}
			}
		}
	}
}
