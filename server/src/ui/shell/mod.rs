//! The signed-in frame around every team page: sidebar, header with breadcrumbs, the
//! command palette, the mobile tab bar, flash messages. `layout::app_page` is the entry
//! point pages use.
//!
//! - `nav`: the list of places (one source for the sidebar, the tab bar and the palette);
//! - `sidebar`: brand, team switcher, navigation and user menu; and the tab bar;
//! - `header`: breadcrumbs with switchers, the search button, the theme button;
//! - `palette`: the search dialog (results come from `pages::search`);
//! - `prefs`: the cookie the frame reads (flash).

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

pub use header::theme_flip;
pub(super) use prefs::pending_flash;
pub use prefs::{flash, scope};
pub use sidebar::brand;

/// The page head additions for a shell page: hold the first paint until `<main>` is parsed,
/// so the cross-document view transition never fades to a half-built page.
fn head_extra() -> Markup {
	html! { link rel="expect" href="#main" blocking="render"; }
}

/// A team page: the frame around `body`. `breadcrumbs` run from the section down to the
/// current page; the shell adds the team (and, on a project's pages, the environment).
pub fn app_page(ctx: &TeamCtx, title: &str, breadcrumbs: &[Crumb<'_>], body: &Markup) -> Markup {
	layout::document_with_head(
		title,
		&head_extra(),
		&html! {
			div class="ui-shell" {
				a href="#main" class="ui-skip" { "Skip to content" }
				(sidebar::aside(ctx))
				div class="ui-shell-main" {
					(header::bar(ctx, breadcrumbs))
					main id="main" tabindex="-1" class="ui-page" { (body) }
				}
				(sidebar::tab_bar(ctx))
				(palette::dialog(ctx))
			}
		},
	)
}
