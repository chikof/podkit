//! The settings pages share one frame, like Quiet's `settings.html`: a page head, a column of
//! sections (Team, Members, Registration, Hugging Face token, Appearance) beside the page's
//! cards. Members and the Hugging Face token keep their own modules and routes; they only
//! use the frame.

use maud::{Markup, html};

use crate::ui::components::icon;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::shell::nav;

/// The column of settings sections for the page being shown. The current one is the one
/// whose address the request is on. On a small screen it becomes a row that scrolls sideways.
pub fn settings_nav(ctx: &TeamCtx) -> Markup {
	let items = nav::settings_items(ctx);
	let current = nav::current(&items, &ctx.path);
	html! {
		nav class="ui-snav" aria-label="Settings sections" {
			ul {
				@for item in &items {
					li {
						a href=(item.href) class="ui-snav-link"
							aria-current=[(current == Some(item.href.as_str())).then_some("page")] {
							(icon(item.icon))
							span { (item.label) }
						}
					}
				}
			}
		}
	}
}

/// The inside of a settings page: the head, the sections and `content`. The page's one `<h1>`
/// is "Settings"; each card names itself with an `<h2>`.
pub fn settings_body(ctx: &TeamCtx, content: &Markup) -> Markup {
	html! {
		header class="ui-page-head" {
			div class="ui-page-title" {
				p class="ui-eyebrow" { "Team" }
				h1 { "Settings" }
				p class="ui-lede" { "Team " (ctx.team.name) " · manage who can deploy, view and administer" }
			}
		}
		div class="ui-settings" {
			(settings_nav(ctx))
			div class="ui-settings-main" { (content) }
		}
	}
}

/// The breadcrumbs of a settings page: Settings, then the page (none for the team page).
pub fn settings_crumbs<'a>(ctx: &TeamCtx, title: &'a str) -> Vec<Crumb<'a>> {
	if title == "Team" {
		vec![("Settings", None)]
	} else {
		vec![
			("Settings", Some(format!("/teams/{}/settings", ctx.team.id))),
			(title, None),
		]
	}
}

/// A settings page: the frame around `content`, with its breadcrumbs.
pub fn settings_page(ctx: &TeamCtx, title: &str, content: &Markup) -> Markup {
	app_page(
		ctx,
		title,
		&settings_crumbs(ctx, title),
		&settings_body(ctx, content),
	)
}
