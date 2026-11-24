//! Registries (Infrastructure): the container-registry logins a team has saved, as cards and
//! an add form with a login test. Image applications, stacks and templates pick one instead
//! of pasting credentials again.
//!
//! The password is write-only: the forms post it once, and no page, signal or response
//! carries it back. "Test login" in the add form sends the same signals as "Add" and stores
//! nothing; a saved registry's own test button never receives the password at all.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::components::{
	BUTTON, BUTTON_OUTLINE, INPUT, LABEL, Size, Variant, button_class, card, card_content,
	card_footer, card_section, dialog, empty_state, icon,
};
use crate::extract::TeamCtx;
use crate::layout::{app_page, form_error};
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{RegistryId, TeamId};
use podkit_core::registries::Registry;
use podkit_core::registries::service::{self, CreateInput, LoginOutcome, UpdateInput};
use podkit_core::state::AppState;

#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/registries",
			get(registries_page).post(create),
		)
		.route("/teams/{team_id}/registries/test", post(test_new))
		.route(
			"/teams/{team_id}/registries/{registry_id}/test",
			post(test_saved),
		)
		.route("/teams/{team_id}/registries/{registry_id}/edit", post(edit))
		.route(
			"/teams/{team_id}/registries/{registry_id}/delete",
			post(remove),
		)
}

fn registries_url(team: TeamId) -> String {
	format!("/teams/{team}/registries")
}

fn mark() -> Markup {
	html! { span class="ui-pick-icon" aria-hidden="true" { (icon("package")) } }
}

// --- the page -----------------------------------------------------------------------------

fn test_id(registry: RegistryId) -> String {
	format!("registry-test-{registry}")
}

/// The line a login test answers with, under its button. Patched in place by id; empty and
/// hidden until a test has run.
fn test_line(id: &str, outcome: Option<&LoginOutcome>) -> Markup {
	let tone = outcome.map(|o| {
		if o.ok {
			"text-success"
		} else {
			"text-destructive"
		}
	});
	html! {
		p id=(id) class={ "text-sm " (tone.unwrap_or_default()) } role="status" hidden[outcome.is_none()] {
			@if let Some(o) = outcome { (o.message) }
		}
	}
}

fn registry_card(
	team: TeamId,
	registry: &Registry,
	used_by: i64,
	can_edit: bool,
	can_delete: bool,
) -> Markup {
	let id = registry.id;
	let open_delete = format!(
		"$_del_id = {}; $_del_name = {}; $_delete_error = ''; el.commandForElement || document.getElementById('delete-registry-dialog').showModal()",
		js_string(&id.to_string()),
		js_string(&registry.name)
	);
	let open_edit = format!(
		"$_ed_id = {}; $ed_name = {}; $ed_username = {}; $ed_password = ''; $_edit_error = ''; el.commandForElement || document.getElementById('edit-registry-dialog').showModal()",
		js_string(&id.to_string()),
		js_string(&registry.name),
		js_string(&registry.username)
	);
	let test = format!(
		"@post({})",
		js_string(&format!("{}/{id}/test", registries_url(team)))
	);
	html! {
		article class="ui-card ui-dest" {
			div class="ui-dest-head" {
				(mark())
				div class="ui-dest-id" {
					h2 title=(registry.name) { (registry.name) }
					p { (registry.url) }
				}
				@if can_delete {
					button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete " (registry.name) }
						command="show-modal" commandfor="delete-registry-dialog" aria-haspopup="dialog" "data-on:click"=(open_delete) {
						(icon("trash-2"))
					}
				}
			}
			dl class="ui-dest-facts" {
				dt { "Registry" }
				dd class="ui-mono" { (registry.url) }
				dt { "Username" }
				dd class="ui-mono" { (registry.username) }
				dt { "Used by" }
				dd {
					@if used_by == 0 { "Nothing yet" }
					@else if used_by == 1 { "1 application or stack" }
					@else { (used_by) " applications and stacks" }
				}
			}
			(test_line(&test_id(id), None))
			@if can_edit {
				div class="flex flex-wrap gap-2" {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(test)
						"data-indicator:_testing" "data-attr:disabled"="$_testing" {
						(icon("zap"))
						"Test login"
					}
					button type="button" class=(BUTTON_OUTLINE) command="show-modal" commandfor="edit-registry-dialog"
						aria-haspopup="dialog" "data-on:click"=(open_edit) {
						(icon("pencil"))
						"Edit"
					}
				}
			}
		}
	}
}

fn delete_dialog(team: TeamId) -> Markup {
	let action = format!(
		"@post({} + $_del_id + '/delete')",
		js_string(&format!("{}/", registries_url(team)))
	);
	dialog(
		"delete-registry-dialog",
		"Delete this registry?",
		Some("Applications and stacks that pull with it must be moved to another registry first."),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Delete " strong class="break-all" "data-text"="$_del_name" {} "?" }
				p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Keep it" }
			}
			button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(action)
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete registry" }
		},
	)
}

fn edit_dialog(team: TeamId) -> Markup {
	let action = format!(
		"evt.preventDefault(); @post({} + $_ed_id + '/edit')",
		js_string(&format!("{}/", registries_url(team)))
	);
	dialog(
		"edit-registry-dialog",
		"Edit registry",
		Some("The address cannot change: applications pull from it by name."),
		&html! {
			form id="edit-registry" class="flex flex-col gap-4" "data-on:submit"=(action) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus "data-bind:ed_name";
				}
				label class=(LABEL) {
					"Username"
					input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" autocapitalize="off" "data-bind:ed_username";
				}
				label class=(LABEL) {
					"New password or token"
					input class=(INPUT) type="password" autocomplete="new-password" placeholder="Leave empty to keep the stored one" "data-bind:ed_password";
					span class="ui-hint" { "Stored encrypted and never shown again." }
				}
				p class="text-sm text-destructive" role="alert" "data-show"="$_edit_error" "data-text"="$_edit_error" style="display: none" {}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) { "Cancel" }
			}
			button type="submit" form="edit-registry" class=(BUTTON) "data-indicator:_editing" "data-attr:disabled"="$_editing" { "Save" }
		},
	)
}

fn add_card(team: TeamId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&registries_url(team))
	);
	let test = format!(
		"@post({})",
		js_string(&format!("{}/test", registries_url(team)))
	);
	card(&html! {
		form id="add-registry" "data-on:submit"=(post) {
			(card_section("Add a registry", Some("The password is stored encrypted and never shown again."), None))
			(card_content(&html! {
				div class="ui-pair" {
					label class=(LABEL) {
						"Name"
						input class=(INPUT) type="text" required maxlength="60" placeholder="GitHub packages" autocomplete="off" "data-bind:name";
					}
					label class=(LABEL) {
						"Registry"
						input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" autocapitalize="off" inputmode="url"
							placeholder="ghcr.io" "data-bind:url";
						span class="ui-hint" { "The host images are pulled from, such as ghcr.io or registry.example.com:5000." }
					}
				}
				div class="ui-pair" {
					label class=(LABEL) {
						"Username"
						input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" autocapitalize="off" "data-bind:username";
					}
					label class=(LABEL) {
						"Password or token"
						input class=(INPUT) type="password" required autocomplete="new-password" spellcheck="false" "data-bind:password";
					}
				}
				p role="status" class="text-sm" "data-show"="$_test" "data-text"="$_test" style="display: none"
					"data-class:text-success"="$_test_ok" "data-class:text-destructive"="!$_test_ok" {}
				(form_error())
			}))
			(card_footer(&html! {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(test) "data-indicator:_testing"
					"data-attr:disabled"="$_testing" { (icon("zap")) "Test login" }
				button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add registry" }
			}))
		}
	})
}

async fn registries_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let rows = service::list_with_usage(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Registry);
	let can_edit = ctx.perms.can(Action::Update, Resource::Registry);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Registry);
	let signals = json!({
		"name": "", "url": "", "username": "", "password": "",
		"ed_name": "", "ed_username": "", "ed_password": "",
		"_test": "", "_test_ok": false, "_error": "", "_delete_error": "", "_edit_error": "",
		"_del_id": "", "_del_name": "", "_ed_id": "",
	})
	.to_string();

	let body = html! {
		div class="ui-dests" "data-signals"=(signals) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Registries" }
					p class="ui-lede" {
						"Logins for private container registries such as GitHub Packages or your own. Save one here and pick it on an image application, a stack or a template."
					}
				}
			}
			@if rows.is_empty() {
				(empty_state(
					"package",
					"No registries yet.",
					Some(if can_create { "Add a login below, test it, and pick it when you create an image application." } else { "Ask someone who can manage infrastructure to add one." }),
					None,
				))
			} @else {
				div class="ui-dest-grid" {
					@for (registry, used_by) in &rows {
						(registry_card(ctx.team.id, registry, *used_by, can_edit, can_delete))
					}
				}
			}
			@if can_create { (add_card(ctx.team.id)) }
			@if can_delete { (delete_dialog(ctx.team.id)) }
			@if can_edit { (edit_dialog(ctx.team.id)) }
		}
	};
	Ok(app_page(&ctx, "Registries", &[("Registries", None)], &body))
}

// --- actions ------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct RegistrySignals {
	name: String,
	url: String,
	username: String,
	password: String,
}

impl RegistrySignals {
	fn input(self) -> CreateInput {
		CreateInput {
			name: self.name.trim().to_owned(),
			url: self.url,
			username: self.username.trim().to_owned(),
			password: self.password,
		}
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<RegistrySignals>,
) -> Response {
	match service::create(&s, ctx.user.id, ctx.team.id, sig.input()).await {
		Ok(_) => sse::redirect(&registries_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// Tries the typed login and stores nothing. The answer is one line under the form; it never
/// repeats the password.
async fn test_new(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<RegistrySignals>,
) -> Response {
	let (text, ok) = match service::test_new(&s, ctx.user.id, ctx.team.id, None, sig.input()).await
	{
		Ok(outcome) => (outcome.message, outcome.ok),
		Err(e) => (error::message(&e), false),
	};
	sse::signals(&json!({ "_test": text, "_test_ok": ok, "_error": "" })).into_response()
}

/// Tries a saved login from the team's local server and answers on the card.
async fn test_saved(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, registry)): Path<(TeamId, RegistryId)>,
) -> Response {
	let outcome = match service::test(&s, ctx.user.id, ctx.team.id, registry, None).await {
		Ok(outcome) => outcome,
		Err(e) => LoginOutcome {
			ok: false,
			message: error::message(&e),
		},
	};
	sse::patch(&test_line(&test_id(registry), Some(&outcome))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EditSignals {
	#[serde(rename = "ed_name")]
	name: String,
	#[serde(rename = "ed_username")]
	username: String,
	#[serde(rename = "ed_password")]
	password: String,
}

async fn edit(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, registry)): Path<(TeamId, RegistryId)>,
	ReadSignals(sig): ReadSignals<EditSignals>,
) -> Response {
	let input = UpdateInput {
		name: sig.name.trim().to_owned(),
		username: sig.username.trim().to_owned(),
		password: Some(sig.password).filter(|p| !p.is_empty()),
	};
	match service::update(&s, ctx.user.id, ctx.team.id, registry, input).await {
		Ok(_) => sse::redirect(&registries_url(ctx.team.id)).into_response(),
		Err(e) => sse::signals(&json!({ "_edit_error": error::message(&e), "ed_password": "" }))
			.into_response(),
	}
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, registry)): Path<(TeamId, RegistryId)>,
) -> Response {
	match service::remove(&s.db, ctx.user.id, ctx.team.id, registry).await {
		Ok(_) => sse::redirect(&registries_url(ctx.team.id)).into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}
