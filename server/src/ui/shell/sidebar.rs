//! The left navigation: team switcher on top, the nav, the user menu at the bottom.
//!
//! The same content is rendered twice: as the desktop `<aside>` (which folds to icons) and
//! inside a modal `<dialog>` that serves as the drawer on small screens. Ids are
//! prefixed so the two copies never clash.

use maud::{Markup, html};

use super::nav::{self, NavItem};
use crate::ui::components::{
	MENU_ITEM, Size, Variant, avatar, button_class, dropdown_menu, icon, menu_label, menu_link,
	menu_separator,
};
use crate::ui::extract::TeamCtx;
use crate::ui::theme::{self, Theme};

const TEAM_TRIGGER: &str = "ui-team-trigger";
const USER_TRIGGER: &str = "ui-user-trigger";

fn link(item: &NavItem, current: bool) -> Markup {
	html! {
		li {
			a href=(item.href) class="ui-nav-link" title=(item.label) aria-current=[current.then_some("page")] {
				(icon(item.icon))
				span class="ui-nav-label" { (item.label) }
			}
		}
	}
}

fn group(heading: &str, items: &[NavItem], current: Option<&str>) -> Markup {
	html! {
		@if !items.is_empty() {
			div class="flex flex-col gap-1" {
				p class="ui-nav-heading px-2 text-xs font-medium text-muted-foreground" { (heading) }
				ul class="flex flex-col gap-0.5" {
					@for item in items {
						(link(item, current == Some(item.href.as_str())))
					}
				}
			}
		}
	}
}

/// The team's name as a button that lists the user's teams. `id` keeps the two copies of
/// the sidebar apart.
fn team_menu(ctx: &TeamCtx, id: &str) -> Markup {
	let trigger = html! {
		span class="ui-team-mark" aria-hidden="true" { (initial(&ctx.team.name)) }
		span class="ui-nav-label min-w-0 flex-1 truncate text-left text-sm font-medium" { (ctx.team.name) }
		span class="ui-nav-label text-muted-foreground" { (icon("chevrons-up-down")) }
	};
	dropdown_menu(
		&format!("{id}-team-menu"),
		TEAM_TRIGGER,
		&format!("Team {}: switch team", ctx.team.name),
		&trigger,
		&team_items(ctx),
	)
}

/// The list inside a team switcher, shared by the sidebar and the breadcrumb.
pub fn team_items(ctx: &TeamCtx) -> Markup {
	html! {
		(menu_label("Teams"))
		@for team in &ctx.teams {
			(menu_link(
				&format!("/teams/{}/projects", team.id.0),
				&html! {
					span class="min-w-0 flex-1 truncate" { (team.name) }
					@if team.id == ctx.team.id { (icon("check")) }
				},
			))
		}
		(menu_separator())
		(menu_link("/teams/new", &html! { (icon("plus")) "New team" }))
	}
}

fn initial(name: &str) -> String {
	name.chars()
		.find(|c| c.is_alphanumeric())
		.map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

fn user_menu(ctx: &TeamCtx, id: &str) -> Markup {
	let current = theme::current();
	let trigger = html! {
		(avatar(&ctx.user.name))
		span class="ui-nav-label min-w-0 flex-1 truncate text-left text-sm font-medium" { (ctx.user.name) }
		span class="ui-nav-label text-muted-foreground" { (icon("chevrons-up-down")) }
	};
	let theme_item = |choice: Theme, label: &str, icon_name: &str| {
		html! {
			button type="submit" name="theme" value=(choice.as_str()) class=(MENU_ITEM) {
				(icon(icon_name))
				span class="flex-1 text-left" { (label) }
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
				span class="flex-1 text-left" { "Sign out" }
			}
		}
	};
	dropdown_menu(
		&format!("{id}-user-menu"),
		USER_TRIGGER,
		&format!("Account menu for {}", ctx.user.name),
		&trigger,
		&items,
	)
}

/// Everything inside the sidebar, for the aside and for the drawer.
pub fn content(ctx: &TeamCtx, id: &str) -> Markup {
	let main = nav::main_items(ctx);
	let settings = nav::settings_items(ctx);
	let current = nav::current(main.iter().chain(&settings), &ctx.path);
	html! {
		div class="flex h-full min-h-0 flex-col" {
			div class="p-2" { (team_menu(ctx, id)) }
			nav aria-label="Main" class="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-2" {
				(group("Platform", &main, current))
				(group("Settings", &settings, current))
			}
			div class="border-t border-sidebar-border p-2" { (user_menu(ctx, id)) }
		}
	}
}

/// The desktop sidebar. It folds to icons through the shell's `data-collapsed`.
pub fn aside(ctx: &TeamCtx) -> Markup {
	html! {
		aside class="ui-sidebar ui-vt-chrome" aria-label="Sidebar" {
			(content(ctx, "side"))
		}
	}
}

/// The small-screen drawer: a modal dialog with the same content and a close button.
pub fn drawer(ctx: &TeamCtx) -> Markup {
	html! {
		dialog id="nav-drawer" class="ui-drawer" closedby="any" aria-label="Navigation" {
			div class="ui-drawer-sheet" {
				form method="dialog" class="ui-drawer-close" {
					button class=(button_class(Variant::Ghost, Size::Icon)) aria-label="Close menu" autofocus {
						(icon("x"))
					}
				}
				(content(ctx, "drawer"))
			}
		}
	}
}
