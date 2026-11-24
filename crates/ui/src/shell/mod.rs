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

use super::components::discard_confirm;
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
use prefs::sidebar_collapsed;
pub use prefs::{flash, scope};
pub use sidebar::brand;

/// The page head additions for a shell page: hold the first paint until `<main>` is parsed,
/// so the cross-document view transition never fades to a half-built page.
fn head_extra() -> Markup {
	html! { link rel="expect" href="#main" blocking="render"; }
}

/// The sidebar's fold state is the client-only `_side` signal: the toggle and the `[` key flip
/// it, `data-class` puts the class on the shell (the server renders the same class from the
/// cookie, so nothing moves on load) and the effect keeps the cookie the server reads.
const SIDE_KEY: &str = "evt.key === '[' && !evt.ctrlKey && !evt.metaKey && !evt.altKey \
	&& !evt.target.closest('input,textarea,select,[contenteditable]') \
	&& !document.querySelector('dialog[open]') && ($_side = !$_side)";
const SIDE_COOKIE: &str = "document.cookie = 'sidebar=' + ($_side ? 'collapsed' : 'open') + '; path=/; max-age=31536000; samesite=lax'";

/// A team page: the frame around `body`. `breadcrumbs` run from the section down to the
/// current page; the shell adds the team (and, on a project's pages, the environment).
pub fn app_page(ctx: &TeamCtx, title: &str, breadcrumbs: &[Crumb<'_>], body: &Markup) -> Markup {
	let collapsed = sidebar_collapsed();
	layout::document_with_head(
		title,
		&head_extra(),
		&html! {
			div class={ "ui-shell" @if collapsed { " ui-shell-collapsed" } }
				"data-signals"={ "{_side: " (collapsed) "}" } "data-class:ui-shell-collapsed"="$_side"
				"data-effect"=(SIDE_COOKIE) "data-on:keydown__window"=(SIDE_KEY) {
				a href="#main" class="ui-skip" { "Skip to content" }
				(sidebar::aside(ctx))
				div class="ui-shell-main" {
					(header::bar(ctx, breadcrumbs))
					main id="main" tabindex="-1" class="ui-page" { (body) }
				}
				(sidebar::tab_bar(ctx))
				(palette::dialog(ctx))
				(discard_confirm())
			}
		},
	)
}
