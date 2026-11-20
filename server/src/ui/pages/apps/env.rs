//! The Environment tab: variable names, never values.
//!
//! Variables take effect on the next deploy, so every change shows the same "Saved" banner
//! with a Redeploy button. Values are write-only: the forms send them, and nothing here (HTML,
//! signal patch or SSE event) ever carries one back. The list shows keys only.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{AppPath, Tab, app_url, close_dialog, closer, load, page, saved_slot};
use crate::auth::permission::{Action, Resource};
use crate::env_vars::service::{self as env_vars, BulkReport, EnvVarKey};
use crate::error::ServerError;
use crate::ids::{ApplicationId, EnvVarId, ProjectId, TeamId};
use crate::shared_env;
use crate::shared_env::service::Inherited;
use crate::state::AppState;
use crate::ui::components::{
	INPUT, LABEL, Size, TEXTAREA, Tone, Variant, badge, button_class, card, card_content,
	card_section, dialog, dialog_cancel, dialog_open_button, empty_state, icon,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/env",
			get(env_page).post(env_set),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/env/bulk",
			post(env_bulk),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/env/{env_var_id}/delete",
			post(env_remove),
		)
}

const ROW: &str = "ui-card-row";

fn env_list(base: &str, keys: &[EnvVarKey], can_delete: bool) -> Markup {
	html! {
		div id="env-list" {
			@if keys.is_empty() {
				(empty_state(
					"key-round",
					"No environment variables yet.",
					Some("Add the settings and secrets your application reads at start. Values are encrypted and never shown again."),
					None,
				))
			} @else {
				ul class="ui-card-rows" aria-label="Environment variables" {
					@for var in keys {
						li class=(ROW) {
							div class="flex min-w-0 flex-col" {
								code class="font-mono text-sm font-medium break-all" { (var.key) }
								span class="text-xs text-muted-foreground" { "Value hidden" }
							}
							@if can_delete {
								button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete " (var.key) }
									"data-on:click"={ "@post(" (js_string(&format!("{base}/env/{}/delete", var.id))) ")" } {
									(icon("trash-2"))
								}
							}
						}
					}
				}
			}
		}
	}
}

/// The variables this application inherits from its team, project and environment: name,
/// where each comes from and whether the application's own variable replaces it. Never values.
fn inherited_card(ctx: &TeamCtx, inherited: &[Inherited]) -> Markup {
	let manage = format!("/teams/{}/settings/shared-variables", ctx.team.id);
	html! {
		(card(&html! {
			(card_section(
				"Inherited variables",
				Some("Set once for the team, project or environment. A variable of the same name set here wins."),
				Some(&html! {
					a class=(button_class(Variant::Outline, Size::Sm)) href=(manage) { (icon("layers")) "Shared variables" }
				}),
			))
			div class="ui-card-list" {
				div id="inherited-list" {
					@if inherited.is_empty() {
						(empty_state("layers", "Nothing inherited.", Some("Shared variables set for the team, project or environment show up here."), None))
					} @else {
						ul class="ui-card-rows" aria-label="Inherited variables" {
							@for var in inherited {
								li class=(ROW) {
									div class="flex min-w-0 flex-col" {
										code class="font-mono text-sm font-medium break-all" { (var.key) }
										span class="text-xs text-muted-foreground" { "Value hidden" }
									}
									div class="ui-inherited-end" {
										(badge(Tone::Neutral, var.level.label()))
										@if var.overridden { (badge(Tone::Warning, "Overridden here")) }
									}
								}
							}
						}
					}
				}
			}
		}))
	}
}

fn bulk_report(report: Option<&BulkReport>) -> Markup {
	html! {
		div id="bulk-result" {
			@if let Some(report) = report {
				div class="flex flex-col gap-1 text-sm" role="status" {
					p {
						"Saved " (report.saved) @if report.saved == 1 { " variable." } @else { " variables." }
					}
					@if !report.errors.is_empty() {
						p class="text-destructive" { "These lines were skipped. Fix them and paste again:" }
						ul class="list-disc pl-5 text-destructive" {
							@for e in &report.errors {
								li { "Line " (e.line) ": " (e.message) "." }
							}
						}
					}
				}
			}
		}
	}
}

fn bulk_dialog(base: &str) -> Markup {
	let post = format!("@post({})", js_string(&format!("{base}/env/bulk")));
	dialog(
		"env-bulk",
		"Edit as .env",
		Some(
			"One KEY=VALUE per line. Blank lines and lines starting with # are ignored. Lines that don't parse are skipped and reported by number.",
		),
		&html! {
			div class="flex flex-col gap-4" {
				(closer("env-bulk"))
				textarea class={ (TEXTAREA) " min-h-56" } rows="10" autocomplete="off" spellcheck="false" autofocus
					aria-label="Contents of a .env file" placeholder="KEY=value" "data-bind:env_bulk" {}
				p class="text-sm text-destructive" role="alert" "data-show"="$_bulk_error" "data-text"="$_bulk_error" style="display: none" {}
				(bulk_report(None))
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post)
				"data-indicator:_bulk_busy" "data-attr:disabled"="$_bulk_busy" { "Import variables" }
		},
	)
}

async fn env_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let keys = env_vars::list(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let inherited =
		shared_env::service::inherited(&s, ctx.user.id, ctx.team.id, project.id, app.id)
			.await
			.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::EnvVar);
	let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);

	let set = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/env"))
	);
	let bulk_button = can_create.then(|| {
		dialog_open_button(
			"env-bulk",
			&button_class(Variant::Outline, Size::Sm),
			&html! { (icon("file-text")) "Edit as .env" },
		)
	});
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(json!({
			"env_key": "", "env_value": "", "env_bulk": "", "_error": "", "_bulk_error": "", "_bulk_busy": false,
		}).to_string()) {
			(saved_slot(&ctx, &base, false))
			(card(&html! {
				(card_section(
					"Environment variables",
					Some("Values are encrypted and never shown again. Setting a name that exists replaces its value."),
					bulk_button.as_ref(),
				))
				div class="ui-card-list" {
					div class="ui-card-slot" { (form_error()) }
					(env_list(&base, &keys, can_delete))
				}
			}))
			(inherited_card(&ctx, &inherited))
			@if can_create {
				(card(&html! {
					(card_section("Add or update a variable", None, None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(set) {
							label class=(LABEL) {
								"Name"
								input class={ (INPUT) " font-mono" } type="text" required maxlength="255" placeholder="DATABASE_URL" autocomplete="off" spellcheck="false" "data-bind:env_key";
							}
							label class=(LABEL) {
								"Value"
								textarea class=(TEXTAREA) rows="3" autocomplete="off" spellcheck="false" "data-bind:env_value" {}
							}
							div {
								button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save variable" }
							}
						}
					}))
				}))
				(bulk_dialog(&base))
			}
		}
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Env, &content).await)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EnvSignals {
	env_key: String,
	env_value: String,
}

async fn env_set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<EnvSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let result = async {
		env_vars::upsert(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			sig.env_key.trim(),
			&sig.env_value,
		)
		.await?;
		env_vars::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(keys) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			sse::patch(&env_list(&base, &keys, can_delete))
				.and(sse::patch(&saved_slot(&ctx, &base, true)))
				.and(sse::signals(
					&json!({ "env_key": "", "env_value": "", "_error": "" }),
				))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BulkSignals {
	env_bulk: String,
}

async fn env_bulk(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<BulkSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let result = async {
		let report =
			env_vars::bulk_set(&s, ctx.user.id, ctx.team.id, project, app, &sig.env_bulk).await?;
		let keys = env_vars::list(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		Ok::<_, ServerError>((report, keys))
	}
	.await;
	match result {
		Ok((report, keys)) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			let mut events = sse::patch(&bulk_report(Some(&report)))
				.and(sse::patch(&env_list(&base, &keys, can_delete)))
				.and(sse::signals(&json!({ "_bulk_error": "" })));
			if report.saved > 0 {
				events = events.and(sse::patch(&saved_slot(&ctx, &base, true)));
			}
			// Keep the text (and the dialog) when lines were skipped, so they can be fixed.
			if report.errors.is_empty() {
				events = events
					.and(sse::signals(&json!({ "env_bulk": "" })))
					.and(close_dialog("env-bulk"));
			}
			events.into_response()
		}
		Err(e) => sse::signals(&json!({ "_bulk_error": error::message(&e) })).into_response(),
	}
}

async fn env_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, EnvVarId)>,
) -> Response {
	let result = async {
		env_vars::remove(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		env_vars::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(keys) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			sse::patch(&env_list(&base, &keys, can_delete))
				.and(sse::patch(&saved_slot(&ctx, &base, true)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}
