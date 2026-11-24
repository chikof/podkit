//! S3 destinations (team settings): the buckets a team's databases are backed up to, as a
//! list of provider cards and an add form with a connection test.
//!
//! The secret key is write-only: the form posts it once, and no page, signal or response
//! ever carries it back. The test button sends the same signals as the add button and stores
//! nothing.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::components::{
	BUTTON, BUTTON_OUTLINE, CHECKBOX, INPUT, LABEL, LogoSize, Size, Variant, button_class, card,
	card_content, card_footer, card_section, dialog, empty_state, icon, logo,
};
use crate::extract::TeamCtx;
use crate::layout::{app_page, form_error};
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::backups::BackupTarget;
use podkit_core::backups::targets::service::{self as targets, CreateInput};
use podkit_core::ids::{BackupTargetId, TeamId};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/backup-targets",
			get(targets_page).post(create_target),
		)
		.route("/teams/{team_id}/backup-targets/test", post(test_target))
		.route(
			"/teams/{team_id}/backup-targets/{target_id}/delete",
			post(remove_target),
		)
}

fn targets_url(team: TeamId) -> String {
	format!("/teams/{team}/backup-targets")
}

// --- providers ----------------------------------------------------------------------------

/// Who runs the bucket, as far as the endpoint says. Only the mark and the name depend on
/// it; every provider speaks the same S3 API.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
	Aws,
	R2,
	Garage,
	Other,
}

impl Provider {
	const ALL: [Self; 4] = [Self::Aws, Self::R2, Self::Garage, Self::Other];

	fn of(endpoint: &str, region: &str) -> Self {
		let host = endpoint.to_ascii_lowercase();
		if host.contains("amazonaws.com") {
			Self::Aws
		} else if host.contains("r2.cloudflarestorage.com") {
			Self::R2
		} else if host.contains("garage") || region.eq_ignore_ascii_case("garage") {
			Self::Garage
		} else {
			Self::Other
		}
	}

	const fn key(self) -> &'static str {
		match self {
			Self::Aws => "aws",
			Self::R2 => "r2",
			Self::Garage => "garage",
			Self::Other => "other",
		}
	}

	const fn label(self) -> &'static str {
		match self {
			Self::Aws => "Amazon S3",
			Self::R2 => "Cloudflare R2",
			Self::Garage => "Garage",
			Self::Other => "Other S3-compatible",
		}
	}

	const fn blurb(self) -> &'static str {
		match self {
			Self::Aws => "Virtual-host addressing",
			Self::R2 => "Region \"auto\"",
			Self::Garage => "Self-hosted, path-style",
			Self::Other => "Any S3 API, path-style",
		}
	}

	/// The mark: a brand logo where podkit has one, else a drawn icon.
	fn mark(self) -> Markup {
		match self {
			Self::Aws => icon_tile("cloud"),
			Self::R2 => logo("cloudflare", LogoSize::Md),
			Self::Garage => logo("Garage", LogoSize::Md),
			Self::Other => icon_tile("hard-drive"),
		}
	}
}

fn icon_tile(name: &str) -> Markup {
	html! { span class="ui-pick-icon" aria-hidden="true" { (icon(name)) } }
}

/// The signals a provider card sets: what is usual for that provider.
fn preset(provider: Provider) -> &'static str {
	match provider {
		Provider::Aws => "$_provider = 'aws'; $region = 'us-east-1'; $path_style = false",
		Provider::R2 => "$_provider = 'r2'; $region = 'auto'; $path_style = false",
		Provider::Garage => "$_provider = 'garage'; $region = 'garage'; $path_style = true",
		Provider::Other => "$_provider = 'other'; $region = 'us-east-1'; $path_style = true",
	}
}

// --- the page -----------------------------------------------------------------------------

fn target_card(ctx: &TeamCtx, target: &BackupTarget, can_delete: bool) -> Markup {
	let provider = Provider::of(&target.endpoint, &target.region);
	let open = format!(
		"$_del_id = {}; $_del_name = {}; $_delete_error = ''; el.commandForElement || document.getElementById('delete-target-dialog').showModal()",
		js_string(&target.id.0.to_string()),
		js_string(&target.name)
	);
	let _ = ctx;
	html! {
		article class="ui-card ui-dest" data-provider=(provider.key()) {
			div class="ui-dest-head" {
				(provider.mark())
				div class="ui-dest-id" {
					h2 title=(target.name) { (target.name) }
					p { (provider.label()) }
				}
				@if can_delete {
					button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete " (target.name) }
						command="show-modal" commandfor="delete-target-dialog" aria-haspopup="dialog" "data-on:click"=(open) {
						(icon("trash-2"))
					}
				}
			}
			dl class="ui-dest-facts" {
				dt { "Endpoint" }
				dd class="ui-mono" title=(target.endpoint) { (target.endpoint) }
				dt { "Bucket" }
				dd { strong { (target.bucket) } " \u{b7} region " (target.region) }
				@if !target.prefix.is_empty() {
					dt { "Prefix" }
					dd class="ui-mono" { (target.prefix) }
				}
				dt { "Access key" }
				dd class="ui-mono" { (target.access_key_id) }
				@if target.path_style {
					dt { "Addressing" }
					dd { "path-style" }
				}
			}
		}
	}
}

fn provider_picker() -> Markup {
	html! {
		fieldset class="ui-choice-grid" {
			legend class="ui-visually-hidden" { "Provider" }
			@for provider in Provider::ALL {
				label class="ui-choice" {
					input type="radio" name="provider" value=(provider.key()) checked[provider == Provider::Aws]
						"data-on:change"=(preset(provider));
					span class="ui-choice-check" aria-hidden="true" { (icon("circle-check")) }
					(provider.mark())
					span class="flex flex-col gap-0.5" {
						span class="text-sm font-medium" { (provider.label()) }
						span class="text-xs text-muted-foreground" { (provider.blurb()) }
					}
				}
			}
		}
	}
}

fn delete_dialog(team: TeamId) -> Markup {
	let action = format!(
		"@post({} + $_del_id + '/delete')",
		js_string(&format!("{}/", targets_url(team)))
	);
	dialog(
		"delete-target-dialog",
		"Delete this destination?",
		Some(
			"Databases using it stop backing up, and backups already made to it can no longer be restored.",
		),
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
				"data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete destination" }
		},
	)
}

fn add_card(team: TeamId) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&targets_url(team))
	);
	let test = format!(
		"@post({})",
		js_string(&format!("{}/test", targets_url(team)))
	);
	let hints = "({aws: 'https://s3.eu-west-1.amazonaws.com', r2: 'https://<account>.r2.cloudflarestorage.com', garage: 'https://s3.garage.example.com', other: 'https://s3.example.com'})[$_provider]";
	card(&html! {
		form id="add-destination" "data-on:submit"=(post) {
			(card_section("Add a destination", Some("The secret key is stored encrypted and never shown again."), None))
			(card_content(&html! {
				(provider_picker())
				div class="ui-pair" {
					label class=(LABEL) {
						"Name"
						input class=(INPUT) type="text" required maxlength="60" placeholder="offsite" autocomplete="off" "data-bind:name";
					}
					label class=(LABEL) {
						"Bucket"
						input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:bucket";
					}
				}
				label class=(LABEL) {
					"Endpoint"
					input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" inputmode="url"
						"data-bind:endpoint" "data-attr:placeholder"=(hints);
					span class="ui-hint" { "Use https. Plain http is only accepted for localhost." }
				}
				div class="ui-pair" {
					label class=(LABEL) {
						"Region"
						input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:region";
					}
					label class=(LABEL) {
						"Prefix (optional)"
						input class=(INPUT) type="text" autocomplete="off" spellcheck="false" placeholder="podkit" "data-bind:prefix";
					}
				}
				label class="ui-check" {
					input type="checkbox" class=(CHECKBOX) "data-bind:path_style";
					"Path-style addressing (S3-compatible servers such as Garage)"
				}
				div class="ui-pair" {
					label class=(LABEL) {
						"Access key ID"
						input class=(INPUT) type="text" required autocomplete="off" spellcheck="false" "data-bind:access_key_id";
					}
					label class=(LABEL) {
						"Secret access key"
						input class=(INPUT) type="password" required autocomplete="new-password" spellcheck="false" "data-bind:secret_access_key";
					}
				}
				p role="status" class="text-sm" "data-show"="$_test" "data-text"="$_test" style="display: none"
					"data-class:text-success"="$_test_ok" "data-class:text-destructive"="!$_test_ok" {}
				(form_error())
			}))
			(card_footer(&html! {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(test) "data-indicator:_testing"
					"data-attr:disabled"="$_testing" { (icon("zap")) "Test connection" }
				button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add destination" }
			}))
		}
	})
}

async fn targets_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<maud::Markup, Response> {
	let rows = targets::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Backup);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Backup);
	let signals = json!({
		"name": "", "endpoint": "", "region": "us-east-1", "bucket": "", "prefix": "",
		"path_style": false, "access_key_id": "", "secret_access_key": "",
		"_provider": "aws", "_test": "", "_test_ok": false,
		"_error": "", "_delete_error": "", "_del_id": "", "_del_name": "",
	})
	.to_string();

	let body = html! {
		div class="ui-dests" "data-signals"=(signals) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "S3 destinations" }
					p class="ui-lede" {
						"S3-compatible buckets (for example Garage, Cloudflare R2 or Amazon S3) that your team's databases and volumes are backed up to."
					}
				}
			}
			@if rows.is_empty() {
				(empty_state(
					"cloud",
					"No destinations yet.",
					Some(if can_create { "Add a bucket below, test it, and pick it on a database's or a volume's backup settings." } else { "Ask someone who can manage backups to set one up." }),
					None,
				))
			} @else {
				div class="ui-dest-grid" {
					@for target in &rows { (target_card(&ctx, target, can_delete)) }
				}
			}
			@if can_create { (add_card(ctx.team.id)) }
			@if can_delete { (delete_dialog(ctx.team.id)) }
		}
	};
	Ok(app_page(
		&ctx,
		"S3 destinations",
		&[("S3 destinations", None)],
		&body,
	))
}

// --- actions ------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct TargetSignals {
	name: String,
	endpoint: String,
	region: String,
	bucket: String,
	prefix: String,
	path_style: bool,
	access_key_id: String,
	secret_access_key: String,
}

impl TargetSignals {
	fn input(self) -> CreateInput {
		CreateInput {
			name: self.name.trim().to_owned(),
			endpoint: self.endpoint,
			region: self.region.trim().to_owned(),
			bucket: self.bucket.trim().to_owned(),
			path_style: self.path_style,
			prefix: self.prefix.trim().to_owned(),
			access_key_id: self.access_key_id.trim().to_owned(),
			secret_access_key: self.secret_access_key,
		}
	}
}

async fn create_target(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<TargetSignals>,
) -> Response {
	match targets::create(&s, ctx.user.id, ctx.team.id, sig.input()).await {
		Ok(_) => sse::redirect(&targets_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// Tries the bucket with the typed keys and stores nothing. The answer is one line under the
/// form; it never repeats a key.
async fn test_target(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<TargetSignals>,
) -> Response {
	let (text, ok) = match targets::test(&s, ctx.user.id, ctx.team.id, sig.input()).await {
		Ok(()) => (
			"Connected. The bucket answered and accepted the keys.".to_owned(),
			true,
		),
		Err(e) => (error::message(&e), false),
	};
	sse::signals(&json!({ "_test": text, "_test_ok": ok, "_error": "" })).into_response()
}

async fn remove_target(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, target)): Path<(TeamId, BackupTargetId)>,
) -> Response {
	match targets::remove(&s.db, ctx.user.id, ctx.team.id, target).await {
		Ok(_) => sse::redirect(&targets_url(ctx.team.id)).into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}
