//! The signed-in frame around every team page: sidebar, header with breadcrumbs, the
//! command palette, flash messages. `layout::app_page` is the entry point pages use.
//!
//! - `nav`: the list of places (one source for the sidebar and the palette);
//! - `sidebar`: team switcher, navigation and user menu, as an aside and as a drawer;
//! - `header`: breadcrumbs with switchers, the search button;
//! - `palette`: the search dialog (results come from `pages::search`);
//! - `prefs`: the cookies the frame reads (folded sidebar, flash).

use maud::{Markup, html};

use super::extract::TeamCtx;
use super::layout::{self, Crumb};

mod header;
pub mod nav;
mod palette;
mod prefs;
mod sidebar;
#[cfg(test)]
mod tests;

pub use prefs::{flash, scope};
pub(super) use prefs::{pending_flash, sidebar_collapsed};

/// The page head additions for a shell page: hold the first paint until `<main>` is parsed,
/// so the cross-document view transition never fades to a half-built page.
fn head_extra() -> Markup {
	html! { link rel="expect" href="#main" blocking="render"; }
}

/// A team page: the frame around `body`. `breadcrumbs` run from the section down to the
/// current page; the shell adds the team (and, on a project's pages, the environment).
pub fn app_page(ctx: &TeamCtx, title: &str, breadcrumbs: &[Crumb<'_>], body: &Markup) -> Markup {
	let collapsed = sidebar_collapsed();
	layout::document_with_head(
		title,
		&head_extra(),
		&html! {
			div class="ui-shell" data-collapsed[collapsed]
				data-signals={ "{_collapsed: " (collapsed) "}" }
				"data-attr:data-collapsed"="$_collapsed"
				"data-on:keydown__window"=(palette::HOTKEY) {
				(sidebar::aside(ctx))
				(sidebar::drawer(ctx))
				(palette::dialog(ctx))
				div class="ui-shell-main" {
					(header::bar(ctx, breadcrumbs))
					main id="main" class="min-w-0 flex-1 px-4 py-6 md:px-8" {
						div class="mx-auto flex max-w-4xl flex-col gap-6" { (body) }
					}
				}
			}
		},
	)
}
