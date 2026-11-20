//! Git providers (Infrastructure → Git providers): the team's GitHub Apps and its GitLab and Gitea servers.
//! Connect one once, then pick a repository from it when creating an application.
//!
//! This is an Operate surface: a table of providers, one menu per row. Everything secret is
//! write-only: a token travels in a signal once and is cleared; a GitHub App's private key never
//! reaches the browser at all (GitHub hands it to podkit's server). Nothing on this page is
//! derived from a stored credential.

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde_json::json;
use time::OffsetDateTime;

use crate::auth::permission::{Action, Resource};
use crate::git_providers::Kind;
use crate::git_providers::service::{self, ProviderView};
use crate::ids::TeamId;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, BannerKind, MENU_ITEM, MENU_ITEM_DANGER, Size, Tone, Variant, banner,
	button_class, dropdown_menu, empty_state, icon, menu_label, menu_separator, table,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::pages::projects::cards::relative_time;
use crate::ui::sse::js_string;

mod actions;
mod dialogs;
pub(super) mod picker;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/git-providers", get(page))
		.route(
			"/teams/{team_id}/git-providers/github/start",
			post(actions::github_start),
		)
		.route(
			"/teams/{team_id}/git-providers/gitlab",
			post(actions::connect_gitlab),
		)
		.route(
			"/teams/{team_id}/git-providers/gitea",
			post(actions::connect_gitea),
		)
		.route(
			"/teams/{team_id}/git-providers/{provider_id}/test",
			post(actions::test),
		)
		.route(
			"/teams/{team_id}/git-providers/{provider_id}/delete",
			post(actions::remove),
		)
		.route(
			"/teams/{team_id}/git-providers/{provider_id}/repos",
			get(picker::repos),
		)
		.route(
			"/teams/{team_id}/git-providers/{provider_id}/branches",
			get(picker::branches),
		)
		.route(
			"/git-providers/github/callback",
			get(actions::github_callback),
		)
		.route(
			"/git-providers/github/installed",
			get(actions::github_installed),
		)
}

fn providers_url(team: TeamId) -> String {
	format!("/teams/{team}/git-providers")
}

fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Git providers", Some(providers_url(ctx.team.id)))]
}

/// The host of an address, for the table: the scheme and path add noise.
fn host_of(base_url: &str) -> String {
	url::Url::parse(base_url)
		.ok()
		.and_then(|u| {
			u.host_str().map(|h| {
				u.port()
					.map_or_else(|| h.to_owned(), |p| format!("{h}:{p}"))
			})
		})
		.unwrap_or_else(|| base_url.to_owned())
}

fn uses_text(p: &ProviderView) -> String {
	let names: Vec<&str> = p.used_by.iter().map(|u| u.name.as_str()).collect();
	names.join(", ")
}

fn used_by(ctx: &TeamCtx, p: &ProviderView) -> Markup {
	let base = format!("/teams/{}", ctx.team.id);
	html! {
		@if p.used_by.is_empty() {
			span class="text-muted-foreground" { "Not used" }
		} @else {
			span class="ui-key-uses" {
				@for (i, app) in p.used_by.iter().enumerate() {
					@if i > 0 { ", " }
					a class="underline underline-offset-4" href={ (base) "/projects/" (app.project_id) "/apps/" (app.id) } { (app.name) }
				}
			}
		}
	}
}

fn row_menu(p: &ProviderView, can_test: bool, can_delete: bool, team: TeamId) -> Markup {
	let test = format!(
		"@post({})",
		js_string(&format!("{}/{}/test", providers_url(team), p.id))
	);
	let delete = format!(
		"$_p_id = {}; $_p_name = {}; $_p_uses = {}; $_delete_error = ''; el.commandForElement || document.getElementById('{}').showModal()",
		js_string(&p.id.0.to_string()),
		js_string(&p.name),
		js_string(&uses_text(p)),
		dialogs::DELETE,
	);
	html! {
		(dropdown_menu(
			&format!("provider-menu-{}", p.id),
			&button_class(Variant::Ghost, Size::Icon),
			&format!("Actions for {}", p.name),
			&icon("ellipsis"),
			&html! {
				(menu_label("Provider"))
				@if can_test {
					button type="button" class=(MENU_ITEM) "data-on:click"=(test) { (icon("circle-check")) "Test connection" }
				}
				@if let Some(install) = p.install_url() {
					a class=(MENU_ITEM) href=(install) target="_blank" rel="noopener noreferrer" {
						(icon("external-link")) "Install on GitHub"
					}
				}
				@if can_delete {
					(menu_separator())
					button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor=(dialogs::DELETE)
						aria-haspopup="dialog" "data-on:click"=(delete) { (icon("trash-2")) "Delete\u{2026}" }
				}
			},
		))
	}
}

fn provider_table(
	ctx: &TeamCtx,
	providers: &[ProviderView],
	can_test: bool,
	can_delete: bool,
) -> Markup {
	let now = OffsetDateTime::now_utc();
	let rows: Vec<Vec<Markup>> = providers
		.iter()
		.map(|p| {
			vec![
				html! { (p.name) },
				html! { span class=(Tone::Neutral.badge_class()) { (p.kind.label()) } },
				html! { span class="ui-key-fp" { (host_of(&p.base_url)) } },
				used_by(ctx, p),
				relative_time(p.created_at, now),
				row_menu(p, can_test, can_delete, ctx.team.id),
			]
		})
		.collect();
	table(
		"provider-list",
		"Git providers",
		&["Name", "Type", "Address", "Used by", "Added", ""],
		&rows,
	)
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let providers = service::list(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::GitProvider);
	let can_update = ctx.perms.can(Action::Update, Resource::GitProvider);
	let can_delete = ctx.perms.can(Action::Delete, Resource::GitProvider);
	let public = s.config.public_url.is_some();

	let signals = json!({
		"gh_name": "", "gh_org": "", "gh_url": "",
		"gl_name": "", "gl_url": "", "gl_token": "",
		"ge_name": "", "ge_url": "", "ge_token": "",
		"_p_id": "", "_p_name": "", "_p_uses": "",
		"_gh_ready": false, "_error": "", "_delete_error": "",
	})
	.to_string();
	// Tokens never outlive the dialog they were typed in.
	let on_close = "$gl_token = ''; $ge_token = ''; $_error = ''; $_gh_ready = false";

	let body = html! {
		div class="ui-page-body" data-signals=(signals) "data-on:close__capture"=(on_close) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Git providers" }
					p class="ui-lede" {
						"Connect GitHub, GitLab or Gitea once, then pick a repository from it when you create an application. Private repositories need no token or deploy key of their own."
					}
				}
				@if can_create {
					div class="ui-page-actions" {
						(dialogs::open_button(dialogs::GITHUB, BUTTON, &html! { (icon("plus")) "Connect GitHub" }))
						(dialogs::open_button(dialogs::GITLAB, BUTTON_OUTLINE, &html! { "GitLab" }))
						(dialogs::open_button(dialogs::GITEA, BUTTON_OUTLINE, &html! { "Gitea" }))
					}
				}
			}
			@if !public {
				(banner(
					BannerKind::Warning,
					Some("podkit has no public address."),
					"Browsers can still connect a provider, but GitHub, GitLab and Gitea cannot call podkit, so a push will not deploy by itself. Deploy from an application's page, or set PUBLIC_URL to the address your providers can reach.",
					None,
				))
			}
			@if providers.is_empty() {
				(empty_state(
					"git-branch",
					"No git providers yet.",
					Some(if can_create {
						"Connect a GitHub App, a GitLab or a Gitea. Applications then clone private repositories through it, and a push can deploy them."
					} else {
						"Ask someone who can manage git providers to connect one."
					}),
					None,
				))
			} @else {
				(provider_table(&ctx, &providers, can_update, can_delete))
			}
			@if can_create {
				(dialogs::github_dialog(ctx.team.id, public))
				(dialogs::token_dialog(ctx.team.id, Kind::Gitlab))
				(dialogs::token_dialog(ctx.team.id, Kind::Gitea))
			}
			@if can_delete { (dialogs::delete_dialog(ctx.team.id)) }
		}
	};
	Ok(app_page(&ctx, "Git providers", &crumbs(&ctx), &body))
}
