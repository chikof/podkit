//! The page frame: the bare HTML document, and the signed-in shell (see `ui::shell`).

use maud::{DOCTYPE, Markup, html};

use super::assets::{FONT_MONO_HREF, FONT_SANS_HREF, PAGE_JS_HREF, PODKIT_JS_HREF};
use super::components::{BUTTON_OUTLINE, icon, toast, toast_region, toast_region_with};
use super::extract::TeamCtx;
use super::{APP_CSS_HREF, DATASTAR_PATH, shell, theme};

/// One breadcrumb: a label and, for every crumb but the last, where it links.
pub type Crumb<'a> = (&'a str, Option<String>);

/// A complete HTML document with no chrome (login page, error fallback).
pub fn document(title: &str, body: &Markup) -> Markup {
	document_with_head(title, &html! {}, body)
}

/// A document with extra elements in `<head>`.
pub fn document_with_head(title: &str, head: &Markup, body: &Markup) -> Markup {
	let theme = theme::current();
	html! {
		(DOCTYPE)
		html lang="en" class=[theme.html_class()] {
			head {
				meta charset="utf-8";
				meta name="viewport" content="width=device-width, initial-scale=1";
				meta name="color-scheme" content=(theme.color_scheme());
				@if cfg!(debug_assertions) {
					// Lets `page.js` honor `?nofeature=` (forces its fallbacks) in a debug build only.
					meta name="podkit-debug" content="1";
				}
				title { (title) " · podkit" }
				link rel="preload" href=(FONT_SANS_HREF) "as"="font" type="font/woff2" crossorigin;
				link rel="preload" href=(FONT_MONO_HREF) "as"="font" type="font/woff2" crossorigin;
				link rel="stylesheet" href=(*APP_CSS_HREF);
				(head)
				script type="module" src=(DATASTAR_PATH) {}
				script type="module" src=(*PODKIT_JS_HREF) {}
				script type="module" src=(*PAGE_JS_HREF) {}
			}
			body class="min-h-screen font-sans" {
				(body)
				@if let Some(flash) = flash_toast() {
					(toast_region_with(&flash))
				} @else {
					(toast_region())
				}
			}
		}
	}
}

/// The message an action left for this page, as a toast. It goes away by itself, except an
/// error, which stays until dismissed.
fn flash_toast() -> Option<Markup> {
	let flash = shell::pending_flash()?;
	let keep = flash.tone == super::components::Tone::Danger;
	Some(html! {
		div class="contents" data-flash "data-init"=(if keep { "" } else { "setTimeout(() => el.remove(), 6000)" }) {
			(toast(flash.tone, &flash.message, None))
		}
	})
}

/// Shows the `_error` signal that action handlers patch on failure.
pub fn form_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_error" "data-text"="$_error" style="display: none" {}
	}
}

/// The `_error` signal as a banner, for the sign-in and sign-up forms: the error sits above
/// the button, with the icon that says "error" without color.
pub fn form_banner() -> Markup {
	html! {
		div class="ui-banner" role="alert" "data-show"="$_error" style="display: none" {
			(icon("circle-alert"))
			span class="ui-banner-body" "data-text"="$_error" {}
		}
	}
}

/// A page on its own, with no sidebar: sign-in, sign-up, registration closed. A centered
/// column holding the brand, `card` and, below it, `meta` (a list of facts). `signals` is the
/// page's initial Datastar signals, if it has a form.
pub fn auth_page(
	title: &str,
	signals: Option<&str>,
	card: &Markup,
	meta: Option<&Markup>,
) -> Markup {
	document(
		title,
		&html! {
			main id="main" class="ui-auth" data-signals=[signals] {
				div class="ui-auth-wrap" {
					section class="ui-auth-card" aria-labelledby="auth-t" {
						(shell::brand("/"))
						(card)
					}
					@if let Some(meta) = meta { (meta) }
				}
				div class="ui-auth-theme" { (shell::theme_flip()) }
			}
		},
	)
}

/// The facts under the sign-in card: which podkit this is and how people sign in to it.
pub fn auth_meta() -> Markup {
	html! {
		dl class="ui-auth-meta" aria-label="This server" {
			div { dt { "Version" } dd { "podkit " (env!("CARGO_PKG_VERSION")) } }
			div { dt { "Sign-in" } dd { "Local password" } }
		}
	}
}

fn sign_out() -> Markup {
	html! {
		form method="post" action="/logout" {
			button type="submit" class=(BUTTON_OUTLINE) { "Sign out" }
		}
	}
}

/// A signed-in page with no team: the brand, the user and Sign out above a page.
pub fn bare_page(user: Option<&str>, title: &str, body: &Markup) -> Markup {
	document(
		title,
		&html! {
			header class="ui-bar ui-bar-bare" {
				(shell::brand("/"))
				div class="ui-bar-actions" {
					@if let Some(name) = user {
						span class="ui-bar-user" { (name) }
					}
					(shell::theme_flip())
					@if user.is_some() { (sign_out()) }
				}
			}
			main id="main" tabindex="-1" class="ui-page ui-page-bare" { (body) }
		},
	)
}

/// A signed-in team page: the shell (sidebar, header, palette) around `body`.
/// `breadcrumbs` run from the section down to the current page.
pub fn app_page(ctx: &TeamCtx, title: &str, breadcrumbs: &[Crumb<'_>], body: &Markup) -> Markup {
	shell::app_page(ctx, title, breadcrumbs, body)
}
