//! The settings pages share one frame: a title, a row of tabs (Team, Members,
//! Registration, Hugging Face token, Appearance) and the page's cards. Members and the
//! Hugging Face token keep their own modules and routes; they only add the tabs.

use maud::{Markup, html};

use crate::ui::components::{TabItem, tabs};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::shell::nav;

/// The tab row for the settings page being shown. The current tab is the one whose
/// address the request is on.
pub fn settings_tabs(ctx: &TeamCtx) -> Markup {
	let items = nav::settings_items(ctx);
	let current = nav::current(&items, &ctx.path);
	let tab_items: Vec<TabItem<'_>> = items
		.iter()
		.map(|i| TabItem {
			label: i.label,
			href: &i.href,
			current: current == Some(i.href.as_str()),
		})
		.collect();
	tabs("Settings sections", &tab_items)
}

/// A settings page: the heading, the tabs, then `content`.
pub fn settings_page(ctx: &TeamCtx, title: &str, content: &Markup) -> Markup {
	let body = html! {
		div class="flex flex-col gap-6" {
			div class="flex flex-col gap-1" {
				h1 class="text-2xl font-semibold tracking-tight" { "Settings" }
				p class="text-sm text-muted-foreground" { "Manage " (ctx.team.name) "." }
			}
			(settings_tabs(ctx))
			(content)
		}
	};
	let home = format!("/teams/{}/settings", ctx.team.id);
	let crumbs: Vec<Crumb<'_>> = if title == "Team" {
		vec![("Settings", None)]
	} else {
		vec![("Settings", Some(home)), (title, None)]
	};
	app_page(ctx, title, &crumbs, &body)
}
