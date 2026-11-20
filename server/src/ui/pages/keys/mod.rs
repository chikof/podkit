//! Keys (Infrastructure → Keys): the team's SSH key vault. Generate or import a key once,
//! pick it for servers and git deploys, rotate it, delete it when nothing uses it.
//!
//! This is an Operate surface: a table of keys, one menu per row. The private half of a key
//! is write-only: an import travels in a signal once and is cleared; nothing on this page, in
//! a signal or in a response is derived from a stored private key, only the public half and
//! the fingerprint are shown.

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde_json::json;
use time::OffsetDateTime;

use crate::auth::permission::{Action, Resource};
use crate::ids::TeamId;
use crate::ssh_keys::service::{self, KeyView};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, MENU_ITEM, MENU_ITEM_DANGER, Size, Tone, Variant, button_class,
	copy_field, dropdown_menu, empty_state, icon, menu_label, menu_separator, table,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::pages::projects::cards::relative_time;
use crate::ui::sse::js_string;

mod actions;
mod dialogs;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/keys", get(page))
		.route("/teams/{team_id}/keys/generate", post(actions::generate))
		.route("/teams/{team_id}/keys/import", post(actions::import))
		.route(
			"/teams/{team_id}/keys/{key_id}/rotate",
			post(actions::rotate),
		)
		.route(
			"/teams/{team_id}/keys/{key_id}/delete",
			post(actions::remove),
		)
}

fn keys_url(team: TeamId) -> String {
	format!("/teams/{team}/keys")
}

fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Keys", Some(keys_url(ctx.team.id)))]
}

/// Who uses a key, as one line: `2 servers, 1 application`, or why it is free.
fn uses_text(key: &KeyView) -> String {
	let names: Vec<&str> = key
		.usage
		.servers
		.iter()
		.chain(&key.usage.applications)
		.map(|u| u.name.as_str())
		.collect();
	names.join(", ")
}

fn used_by(ctx: &TeamCtx, key: &KeyView) -> Markup {
	let base = format!("/teams/{}", ctx.team.id);
	html! {
		@if key.usage.is_empty() {
			span class="text-muted-foreground" { "Not used" }
		} @else {
			span class="ui-key-uses" {
				@for (i, server) in key.usage.servers.iter().enumerate() {
					@if i > 0 { ", " }
					a class="underline underline-offset-4" href={ (base) "/servers/" (server.id) } { (server.name) }
				}
				@for (i, app) in key.usage.applications.iter().enumerate() {
					@if i > 0 || !key.usage.servers.is_empty() { ", " }
					@if let Some(project) = app.project_id {
						a class="underline underline-offset-4" href={ (base) "/projects/" (project) "/apps/" (app.id) } { (app.name) }
					} @else {
						(app.name)
					}
				}
			}
		}
	}
}

fn row_menu(key: &KeyView, can_rotate: bool, can_delete: bool) -> Markup {
	let open = |dialog: &str| {
		format!(
			"$_k_id = {}; $_k_name = {}; $_k_uses = {}; $_rotate_error = ''; $_rotated = false; $_delete_error = ''; el.commandForElement || document.getElementById('{dialog}').showModal()",
			js_string(&key.id.0.to_string()),
			js_string(&key.name),
			js_string(&uses_text(key)),
		)
	};
	let public = dialogs::public_id(key);
	let show_public = format!(
		"el.commandForElement || document.getElementById({}).showModal()",
		js_string(&public)
	);
	html! {
		(dropdown_menu(
			&format!("key-menu-{}", key.id.0),
			&button_class(Variant::Ghost, Size::Icon),
			&format!("Actions for {}", key.name),
			&icon("ellipsis"),
			&html! {
				(menu_label("Key"))
				button type="button" class=(MENU_ITEM) command="show-modal" commandfor=(public)
					aria-haspopup="dialog" "data-on:click"=(show_public) { (icon("copy")) "Copy public key" }
				@if can_rotate {
					button type="button" class=(MENU_ITEM) command="show-modal" commandfor=(dialogs::ROTATE)
						aria-haspopup="dialog" "data-on:click"=(open(dialogs::ROTATE)) { (icon("refresh-cw")) "Rotate\u{2026}" }
				}
				@if can_delete {
					(menu_separator())
					button type="button" class=(MENU_ITEM_DANGER) command="show-modal" commandfor=(dialogs::DELETE)
						aria-haspopup="dialog" "data-on:click"=(open(dialogs::DELETE)) { (icon("trash-2")) "Delete\u{2026}" }
				}
			},
		))
	}
}

fn key_table(ctx: &TeamCtx, keys: &[KeyView]) -> Markup {
	let can_rotate = ctx.perms.can(Action::Update, Resource::SshKey)
		&& ctx.perms.can(Action::Update, Resource::Server);
	let can_delete = ctx.perms.can(Action::Delete, Resource::SshKey);
	let now = OffsetDateTime::now_utc();
	let rows: Vec<Vec<Markup>> = keys
		.iter()
		.map(|key| {
			vec![
				html! { (key.name) },
				html! { span class=(Tone::Neutral.badge_class()) { (key.algorithm_label()) } },
				html! {
					@if key.fingerprint.is_empty() {
						span class="text-muted-foreground" { "Not derived yet" }
					} @else {
						span class="ui-key-fp" { (key.fingerprint) }
					}
				},
				used_by(ctx, key),
				html! {
					@if let Some(at) = key.last_used_at { (relative_time(at, now)) }
					@else { span class="text-muted-foreground" { "Never" } }
				},
				row_menu(key, can_rotate, can_delete),
			]
		})
		.collect();
	table(
		"key-list",
		"SSH keys",
		&["Name", "Type", "Fingerprint", "Used by", "Last used", ""],
		&rows,
	)
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let keys = service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::SshKey);
	let can_rotate = ctx.perms.can(Action::Update, Resource::SshKey)
		&& ctx.perms.can(Action::Update, Resource::Server);
	let can_delete = ctx.perms.can(Action::Delete, Resource::SshKey);

	let signals = json!({
		"gen_name": "", "gen_algorithm": "ed25519", "imp_name": "", "imp_key": "",
		"_k_id": "", "_k_name": "", "_k_uses": "",
		"_gen_done": false, "_imp_done": false, "_rotated": false,
		"_error": "", "_rotate_error": "", "_delete_error": "",
	})
	.to_string();
	// A finished action has changed what the list shows; closing its dialog reloads it.
	let on_close = "($_gen_done || $_imp_done || $_rotated) && window.location.reload(); $imp_key = ''; $_error = ''";

	let body = html! {
		div class="ui-page-body" data-signals=(signals) "data-on:close__capture"=(on_close) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Keys" }
					p class="ui-lede" {
						"SSH keys for this team's servers and git deploys. Make or import a key once, then pick it wherever one is asked for."
					}
				}
				@if can_create {
					div class="ui-page-actions" {
						(dialogs::open_button(dialogs::GENERATE, BUTTON, &html! { (icon("plus")) "Generate key" }))
						(dialogs::open_button(dialogs::IMPORT, BUTTON_OUTLINE, &html! { (icon("upload")) "Import key" }))
					}
				}
			}
			@if keys.is_empty() {
				(empty_state(
					"key-round",
					"No keys yet.",
					Some(if can_create {
						"Generate a key, or import one you already use. Servers log in with it and git repositories are cloned with it."
					} else {
						"Ask someone who can manage keys to add one."
					}),
					None,
				))
			} @else {
				(key_table(&ctx, &keys))
			}
			@for key in &keys { (dialogs::public_dialog(key)) }
			@if can_create {
				(dialogs::generate_dialog(ctx.team.id))
				(dialogs::import_dialog(ctx.team.id))
			}
			@if can_rotate { (dialogs::rotate_dialog(ctx.team.id)) }
			@if can_delete { (dialogs::delete_dialog(ctx.team.id)) }
		}
	};
	Ok(app_page(&ctx, "Keys", &crumbs(&ctx), &body))
}

/// The panel that follows a key being made or imported: what it is and its public half to
/// copy.
fn made_panel(id: &str, key: &KeyView, lead: &str) -> Markup {
	html! {
		div id=(id) class="flex flex-col gap-3" {
			p class="text-sm" { (lead) " " strong { (key.name) } "." }
			dl class="ui-key-facts" {
				dt { "Type" }
				dd { (key.algorithm_label()) }
				dt { "Fingerprint" }
				dd class="ui-key-fp" { (key.fingerprint) }
			}
			(copy_field(&format!("{id}-public"), "Public key", &key.public_key))
		}
	}
}
