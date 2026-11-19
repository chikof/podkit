//! The left column (a brand strip on a small screen): the brand, the team switcher, the
//! navigation and the user menu; and the mobile tab bar, which lists the same places from
//! the same `nav` data.
//!
//! The two navigations are two landmarks, and CSS shows one of them: the sidebar's above
//! 760 px, the tab bar below. Each hidden one is `display: none`, so a screen reader never
//! meets the places twice.

use maud::{Markup, html};

use super::nav::{self, NavItem};
use crate::ui::components::{
	MENU_ITEM, avatar, dropdown_menu, icon, menu_label, menu_link, menu_separator,
};
use crate::ui::extract::TeamCtx;
use crate::ui::theme::{self, Theme};

const TEAM_TRIGGER: &str = "ui-team-trigger";
const USER_TRIGGER: &str = "ui-user-trigger";

/// The podkit mark: a capsule split in two with a dot, drawn with `currentColor`.
fn brand_mark() -> Markup {
	html! {
		svg class="ui-brand-svg" viewBox="0 0 24 24" aria-hidden="true" focusable="false" {
			rect x="2.5" y="6.5" width="19" height="11" rx="5.5" fill="none" stroke="currentColor" stroke-width="2" {}
			path d="M12 6.5v11" stroke="currentColor" stroke-width="2" {}
			circle cx="7.2" cy="12" r="1.6" fill="currentColor" {}
		}
	}
}

fn brand_in(href: &str) -> Markup {
	html! {
		a class="ui-brand" href=(href) {
			span class="ui-brand-mark" { (brand_mark()) }
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

fn link(item: &NavItem, current: bool) -> Markup {
	html! {
		li {
			a href=(item.href) class="ui-nav-link" aria-current=[current.then_some("page")] {
				(icon(item.icon))
				span class="ui-nav-label" { (item.label) }
			}
		}
	}
}

/// The team's name as a button that lists the user's teams.
fn team_menu(ctx: &TeamCtx) -> Markup {
	let trigger = html! {
		span class="ui-team-mark" aria-hidden="true" { (initial(&ctx.team.name)) }
		span class="ui-team-name" { (ctx.team.name) }
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
				span class="ui-team-mark ui-team-mark-sm" aria-hidden="true" { (initial(&team.name)) }
				span class="ui-menu-grow" { (team.name) }
				@if team.id == ctx.team.id { (icon("check")) }
			}
		}
		(menu_separator())
		(menu_link(&format!("/teams/{}/settings", ctx.team.id), &html! { (icon("settings")) "Team settings" }))
		(menu_link("/teams/new", &html! { (icon("plus")) "New team" }))
	}
}

fn initial(name: &str) -> String {
	name.chars()
		.find(|c| c.is_alphanumeric())
		.map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

/// The signed-in user: the avatar and name at the foot of the sidebar. The menu has the
/// theme and Sign out. On a small screen only the avatar stays.
fn user_menu(ctx: &TeamCtx) -> Markup {
	let current = theme::current();
	let trigger = html! {
		(avatar(&ctx.user.name))
		span class="ui-user-name" { (ctx.user.name) }
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

/// The sidebar. Above 760 px it is the left column; below, the strip across the top.
pub fn aside(ctx: &TeamCtx) -> Markup {
	let items = nav::primary_items(ctx);
	let current = nav::primary_current(ctx);
	html! {
		aside class="ui-sidebar ui-vt-chrome" aria-label="Application" {
			(side_brand(ctx))
			(team_menu(ctx))
			nav class="ui-nav" aria-label="Primary" {
				ul class="ui-nav-list" {
					@for item in &items {
						(link(item, current.as_deref() == Some(item.href.as_str())))
					}
				}
			}
			div class="ui-chrome-foot" { (user_menu(ctx)) }
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
					(link(item, current.as_deref() == Some(item.href.as_str())))
				}
			}
		}
	}
}
