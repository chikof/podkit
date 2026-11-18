//! The page frame: the bare HTML document, and the signed-in shell with a
//! header (team switcher, user, sign out) and a sidebar.

use maud::{DOCTYPE, Markup, html};

use super::components::BUTTON_OUTLINE;
use super::extract::TeamCtx;
use super::{APP_CSS_HREF, DATASTAR_PATH};
use crate::auth::permission::{Action, Resource};

/// One breadcrumb: a label and, for every crumb but the last, where it links.
pub type Crumb<'a> = (&'a str, Option<String>);

/// A complete HTML document with no chrome (login page, error fallback).
pub fn document(title: &str, body: &Markup) -> Markup {
	html! {
		(DOCTYPE)
		html lang="en" {
			head {
				meta charset="utf-8";
				meta name="viewport" content="width=device-width, initial-scale=1";
				title { (title) " · podkit" }
				script type="module" src=(DATASTAR_PATH) {}
				link rel="stylesheet" href=(*APP_CSS_HREF);
			}
			body class="min-h-screen font-sans" {
				(body)
				div id="toasts" class="pointer-events-none fixed right-4 bottom-4 z-50 flex flex-col gap-2" {}
			}
		}
	}
}

/// Shows the `_error` signal that action handlers patch on failure.
pub fn form_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_error" "data-text"="$_error" style="display: none" {}
	}
}

fn sign_out() -> Markup {
	html! {
		form method="post" action="/logout" {
			button type="submit" class=(BUTTON_OUTLINE) { "Sign out" }
		}
	}
}

/// A signed-in page with no team: just the brand and Sign out.
pub fn bare_page(user: Option<&str>, title: &str, body: &Markup) -> Markup {
	document(
		title,
		&html! {
			header class="flex items-center justify-between border-b border-border bg-card px-6 py-3" {
				a href="/" class="font-semibold" { "podkit" }
				@if let Some(name) = user {
					div class="flex items-center gap-3" {
						span class="text-sm text-muted-foreground" { (name) }
						(sign_out())
					}
				}
			}
			main class="mx-auto max-w-3xl px-6 py-8" { (body) }
		},
	)
}

const LINK: &str = "block rounded-lg px-3 py-2 text-sm font-medium hover:bg-foreground/5";

/// Sidebar entries the caller may use. Pages for the later ones may not exist
/// yet and answer 404.
fn nav_links(ctx: &TeamCtx) -> Markup {
	let base = format!("/teams/{}", ctx.team.id);
	let perms = &ctx.perms;
	html! {
		@if perms.can(Action::Read, Resource::Project) {
			a class=(LINK) href={ (base) "/projects" } { "Projects" }
		}
		@if perms.can(Action::Read, Resource::Model) {
			a class=(LINK) href={ (base) "/models" } { "Models" }
		}
		@if perms.can(Action::Read, Resource::Server) {
			a class=(LINK) href={ (base) "/servers" } { "Servers" }
		}
		@if perms.can(Action::Read, Resource::Backup) {
			a class=(LINK) href={ (base) "/backup-targets" } { "Backup targets" }
		}
		@if perms.can(Action::Read, Resource::Membership) {
			a class=(LINK) href={ (base) "/members" } { "Members" }
		}
	}
}

fn team_switcher(ctx: &TeamCtx) -> Markup {
	html! {
		details class="relative" {
			summary class="flex h-9 cursor-pointer list-none items-center gap-2 rounded-lg border border-border px-3 text-sm font-medium select-none" {
				(ctx.team.name)
			}
			div class="absolute left-0 z-40 mt-2 flex min-w-48 flex-col rounded-lg border border-border bg-card p-1 text-card-foreground" {
				@for team in &ctx.teams {
					a class=(LINK) href={ "/teams/" (team.id.0) "/projects" } { (team.name) }
				}
				a class=(LINK) href="/teams/new" { "New team" }
			}
		}
	}
}

/// The signed-in shell around `body`. `breadcrumbs` run from the section down
/// to the current page.
pub fn app_page(ctx: &TeamCtx, title: &str, breadcrumbs: &[Crumb<'_>], body: &Markup) -> Markup {
	document(
		title,
		&html! {
			header class="flex items-center justify-between gap-3 border-b border-border bg-card px-4 py-3 md:px-6" {
				div class="flex items-center gap-4" {
					a href="/" class="font-semibold" { "podkit" }
					(team_switcher(ctx))
				}
				div class="flex items-center gap-3" {
					span class="hidden text-sm text-muted-foreground md:inline" { (ctx.user.name) }
					(sign_out())
				}
			}
			div class="md:flex md:min-h-[calc(100vh-61px)]" {
				details class="border-b border-border bg-card px-4 py-2 md:hidden" {
					summary class="cursor-pointer text-sm font-medium select-none" { "Menu" }
					nav class="flex flex-col pt-2" { (nav_links(ctx)) }
				}
				aside class="hidden w-56 shrink-0 border-r border-border bg-card p-3 md:block" {
					nav class="flex flex-col gap-1" { (nav_links(ctx)) }
				}
				main class="min-w-0 flex-1 px-4 py-6 md:px-8" {
					nav class="mb-4 flex flex-wrap items-center gap-2 text-sm text-muted-foreground" aria-label="Breadcrumb" {
						@for (i, (label, href)) in breadcrumbs.iter().enumerate() {
							@if i > 0 { span { "/" } }
							@if let Some(href) = href {
								a class="hover:underline" href=(href) { (label) }
							} @else {
								span class="text-foreground" { (label) }
							}
						}
					}
					div class="mx-auto flex max-w-4xl flex-col gap-6" { (body) }
				}
			}
		},
	)
}
