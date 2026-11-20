//! The actions of the Keys page: generate, import, rotate and delete. Each one goes through
//! the vault's service, which authorizes and scopes it; a forged POST gets the same refusal
//! as the API.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use tower_cookies::Cookies;

use super::{keys_url, made_panel};
use crate::ids::{SshKeyId, TeamId};
use crate::ssh_keys::keygen::Kind;
use crate::ssh_keys::rotate::DEPLOY_KEY_NOTE;
use crate::ssh_keys::rotate::{Outcome, Rotation};
use crate::ssh_keys::service::{self, NewKey, Source};
use crate::state::AppState;
use crate::ui::components::{BannerKind, Tone, banner, copy_field};
use crate::ui::extract::TeamCtx;
use crate::ui::shell;
use crate::ui::{error, sse};

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct GenerateSignals {
	gen_name: String,
	gen_algorithm: String,
}

/// Makes a key and shows its public half. Nothing else about it is ever shown.
pub(super) async fn generate(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<GenerateSignals>,
) -> Response {
	let kind = if sig.gen_algorithm == "rsa" {
		Kind::Rsa
	} else {
		Kind::Ed25519
	};
	let new = NewKey {
		name: sig.gen_name,
		source: Source::Generate(kind),
	};
	match service::create(&s, ctx.user.id, ctx.team.id, new).await {
		Ok(key) => sse::patch(&made_panel("generate-made", &key, "Generated"))
			.and(sse::signals(&json!({ "_gen_done": true, "_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct ImportSignals {
	imp_name: String,
	imp_key: String,
}

/// Imports a pasted key. The text leaves the signals as soon as the key is stored.
pub(super) async fn import(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<ImportSignals>,
) -> Response {
	let new = NewKey {
		name: sig.imp_name,
		source: Source::Import(sig.imp_key),
	};
	match service::create(&s, ctx.user.id, ctx.team.id, new).await {
		Ok(key) => sse::patch(&made_panel("import-made", &key, "Imported"))
			.and(sse::signals(
				&json!({ "_imp_done": true, "_error": "", "imp_key": "" }),
			))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

/// One server's line in the rotation report.
fn outcome_row(name: &str, outcome: &Outcome) -> Markup {
	let (tone, label, message) = match outcome {
		Outcome::Rotated => (Tone::Success, "Rotated", None),
		Outcome::Warning(m) => (Tone::Warning, "Rotated, with a warning", Some(m.as_str())),
		Outcome::Failed(m) => (Tone::Danger, "Kept the old key", Some(m.as_str())),
	};
	html! {
		li {
			span class=(tone.badge_class()) { (label) }
			strong { (name) }
			@if let Some(message) = message { small { (message) } }
		}
	}
}

/// What the rotation did, server by server, and what is left for the user to do.
fn report(team: TeamId, rotation: &Rotation) -> Markup {
	let _ = team;
	html! {
		div id="rotate-result" class="flex flex-col gap-3" {
			@if rotation.servers.is_empty() {
				p class="text-sm text-muted-foreground" { "No server used this key." }
			} @else {
				ul class="ui-key-report" aria-label="Servers" {
					@for server in &rotation.servers { (outcome_row(&server.name, &server.outcome)) }
				}
			}
			@if rotation.new_key.is_none() {
				(banner(
					BannerKind::Error,
					Some("Nothing was moved."),
					"No server could be switched, so the old key is unchanged and no new key was kept.",
					None,
				))
			}
			@if !rotation.applications.is_empty() {
				(banner(
					BannerKind::Warning,
					Some("Git deploy keys are not rotated for you."),
					DEPLOY_KEY_NOTE,
					None,
				))
				p class="text-sm" {
					"Still on the old key: "
					@for (i, app) in rotation.applications.iter().enumerate() {
						@if i > 0 { ", " }
						strong { (app.name) }
					}
					"."
				}
			}
			@if let Some(key) = &rotation.new_key {
				(copy_field("rotate-new-public", &format!("Public key of {}", key.name), &key.public_key))
			}
		}
	}
}

pub(super) async fn rotate(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, SshKeyId)>,
) -> Response {
	match service::rotate(&s, ctx.user.id, ctx.team.id, id).await {
		Ok(rotation) => sse::patch(&report(ctx.team.id, &rotation))
			.and(sse::signals(
				&json!({ "_rotated": true, "_rotate_error": "" }),
			))
			.into_response(),
		Err(e) => sse::signals(&json!({ "_rotate_error": error::message(&e) })).into_response(),
	}
}

pub(super) async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, id)): Path<(TeamId, SshKeyId)>,
) -> Response {
	match service::remove(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Key deleted");
			sse::redirect(&keys_url(ctx.team.id)).into_response()
		}
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}
