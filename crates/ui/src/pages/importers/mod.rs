//! Settings → Import: bring projects over from Coolify, Dokploy or CapRover.
//!
//! One screen, three steps on it: pick the platform and give its address and credential, read it
//! and see what would be made (the report of what did not map, then the plan against this team),
//! apply. Everything goes through [`podkit_core::importers::service`], which authorizes as the JSON API
//! does, so a forged POST is refused the same way.
//!
//! The credential is read once, by the plan. It is cleared from the form as soon as the plan
//! answers and is never stored; the secrets the source held wait, encrypted, in the preview the
//! page keeps an id for, and nothing here ever puts a value on a page.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::iac::plan_view;
use super::settings::{settings_body, settings_crumbs};
use crate::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, SELECT, Size, Variant, button_class, card, card_content,
	card_section, dialog, icon,
};
use crate::extract::TeamCtx;
use crate::layout::{app_page, form_error};
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::iac::service::ImportOutcome;
use podkit_core::importers::service::{self, Request};
use podkit_core::importers::{Credentials, Source};
use podkit_core::servers::{self, ServerStatus};
use podkit_core::state::AppState;

#[cfg(test)]
mod tests;
mod view;

const DIALOG: &str = "import-apply";
const TITLE: &str = "Import";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/settings/import", get(page))
		.route("/teams/{team_id}/settings/import/plan", post(plan))
		.route("/teams/{team_id}/settings/import/apply", post(apply))
}

/// What the form sends. `imp_id` is the preview a plan made; the underscore signals stay in the
/// browser.
#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the `imp_` prefix names the page's signals"
)]
struct Signals {
	imp_source: String,
	imp_url: String,
	imp_token: String,
	imp_server: String,
	imp_deploy: bool,
	imp_id: String,
}

impl Signals {
	fn source(&self) -> Result<Source, ServerError> {
		Source::parse(&self.imp_source).ok_or_else(|| {
			ServerError::Validation("Choose the platform to import from.".to_owned())
		})
	}
}

fn can_import(ctx: &TeamCtx) -> bool {
	ctx.perms.can(Action::Create, Resource::Project)
}

fn blurb(source: Source) -> &'static str {
	match source {
		Source::Coolify => "v4, with an API token",
		Source::Dokploy => "with an API key",
		Source::Caprover => "with the dashboard password",
	}
}

fn source_picker() -> Markup {
	html! {
		fieldset class="ui-choice-grid" {
			legend class="ui-visually-hidden" { "Import from" }
			@for source in Source::ALL {
				label class="ui-choice" {
					input type="radio" name="source" value=(source.as_str()) "data-bind:imp_source";
					span class="ui-choice-check" aria-hidden="true" { (icon("circle-check")) }
					span class="flex flex-col gap-0.5" {
						span class="text-sm font-medium" { (source.label()) }
						span class="text-xs text-muted-foreground" { (blurb(source)) }
					}
				}
			}
		}
	}
}

/// The help under the credential: what to make on each platform, shown for the one picked.
fn credential_help() -> Markup {
	let line = |source: Source, text: &str| {
		html! {
			span class="ui-hint" "data-show"=(format!("$imp_source == '{}'", source.as_str())) style="display: none" {
				(text)
			}
		}
	};
	html! {
		span id="imp-token-help" class="ui-hint" {
			(line(Source::Coolify, "A token with the read and read:sensitive abilities; without read:sensitive the variables come over without values."))
			(line(Source::Dokploy, "An API key from the profile page of a user who can read the projects."))
			(line(Source::Caprover, "The dashboard password. It is used to log in once; podkit only reads."))
			span class="ui-hint" { " It is used for this read only and is never stored." }
		}
	}
}

fn form(servers: &[(String, String)]) -> Markup {
	html! {
		(source_picker())
		div class="ui-field-group" {
			label for="imp-url" class=(LABEL) { "Address" }
			input id="imp-url" name="url" class=(INPUT) type="url" required inputmode="url"
				autocomplete="off" autocapitalize="none" autocorrect="off" spellcheck="false"
				placeholder="https://coolify.example.com" aria-describedby="imp-url-help"
				"data-bind:imp_url";
			span id="imp-url-help" class="ui-hint" { "Where you open the platform in a browser." }
		}
		div class="ui-field-group" {
			label for="imp-token" class=(LABEL) { "Credential" }
			input id="imp-token" name="token" class=(INPUT) type="password" required
				autocomplete="off" autocapitalize="none" autocorrect="off" spellcheck="false"
				aria-describedby="imp-token-help" "data-bind:imp_token";
			(credential_help())
		}
		div class="ui-field-group" {
			label for="imp-server" class=(LABEL) { "Put everything on" }
			div class="ui-select-wrap" {
				select id="imp-server" name="server" class=(SELECT) required aria-describedby="imp-server-help"
					"data-bind:imp_server" {
					button { selectedcontent {} }
					@for (name, _) in servers { option value=(name) { (name) } }
				}
				(icon("chevron-down"))
			}
			span id="imp-server-help" class="ui-hint" {
				"The server the applications and databases run on. Move them later if you need to."
			}
		}
	}
}

fn apply_button() -> Markup {
	html! {
		button type="button" class=(BUTTON) command="show-modal" commandfor=(DIALOG)
			aria-haspopup="dialog"
			"data-on:click"=(format!("el.commandForElement || document.getElementById({}).showModal()", js_string(DIALOG))) {
			(icon("check")) "Import\u{2026}"
		}
	}
}

fn apply_dialog(post_to: &str) -> Markup {
	let post = format!("@post({})", js_string(post_to));
	dialog(
		DIALOG,
		"Import into this team?",
		Some("What the plan shows is made; nothing on the source is changed."),
		&html! {
			div class="flex flex-col gap-3" {
				p class="text-sm" {
					"Variables are stored encrypted, like the ones you type in. Databases start empty: dump the data on the source and restore it here."
				}
				div class="ui-check" {
					input id="imp-deploy" name="deploy" type="checkbox" class=(CHECKBOX) aria-describedby="imp-deploy-help"
						"data-bind:imp_deploy";
					label for="imp-deploy" class="cursor-pointer" { "Deploy after importing" }
				}
				p id="imp-deploy-help" class="ui-hint pl-6" {
					"Queue a deploy of every application and stack that is made. Leave it off to look first."
				}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Not yet" }
			}
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post)
				"data-indicator:_applying" "data-attr:disabled"="$_applying" { "Import" }
		},
	)
}

fn signals(first_server: &str) -> String {
	json!({
		"imp_source": "coolify", "imp_url": "", "imp_token": "", "imp_server": first_server,
		"imp_deploy": false, "imp_id": "",
		"_error": "", "_busy": false, "_applying": false,
	})
	.to_string()
}

async fn active_servers(s: &AppState, ctx: &TeamCtx) -> Result<Vec<(String, String)>, ServerError> {
	Ok(servers::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await?
		.into_iter()
		.filter(|x| x.status == ServerStatus::Active)
		.map(|x| (x.name, x.hostname))
		.collect())
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	if !can_import(&ctx) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let servers = active_servers(&s, &ctx)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let first = servers.first().map(|(n, _)| n.as_str()).unwrap_or_default();
	let base = format!("/teams/{}/settings/import", ctx.team.id);
	let plan_post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/plan"))
	);
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(signals(first)) {
			(card(&html! {
				(card_section(
					"Import from another platform",
					Some("Read a Coolify, Dokploy or CapRover instance, see what podkit would make and what it cannot, then import. The source is only read."),
					None,
				))
				(card_content(&html! {
					@if servers.is_empty() {
						p class="text-sm" { "No server is ready yet. Add one under Servers, or wait until it answers." }
					} @else {
						form id="import-form" class="flex flex-col gap-4" "data-on:submit"=(plan_post) "data-indicator:_busy" {
							(form(&servers))
							(form_error())
							div class="flex justify-end" {
								button type="submit" class=(BUTTON) "data-attr:disabled"="$_busy" {
									(icon("upload")) "Read and plan"
								}
							}
						}
					}
				}))
			}))
			(plan_view_empty())
			(apply_dialog(&format!("{base}/apply")))
		}
	};
	Ok(app_page(
		&ctx,
		TITLE,
		&settings_crumbs(&ctx, TITLE),
		&settings_body(&ctx, &content),
	))
}

fn plan_view_empty() -> Markup {
	html! { div id=(view::RESULT) aria-live="polite" {} }
}

/// The result region: the report, then the plan per project, then the button that imports.
fn previewed(preview: &service::Preview) -> Markup {
	let plans = plan_view::projects_inner(&preview.plans);
	let ok = !preview.has_issues() && preview.plans.iter().any(|p| !p.plan.changes.is_empty());
	html! {
		div id=(view::RESULT) aria-live="polite" class="flex flex-col gap-6" {
			(card(&html! {
				(card_section("What was read", None, None))
				(card_content(&view::report(preview.source, &preview.report)))
			}))
			@if preview.plans.is_empty() {
				(card(&html! {
					(card_content(&html! { p class="text-sm" { "Nothing could be imported. The report above says why." } }))
				}))
			} @else {
				(card(&html! {
					(card_section("What would be made", Some("Planned against this team. Nothing is made until you import."), None))
					(card_content(&html! {
						(plans)
						@if ok { div class="ui-iac-foot" { (apply_button()) } }
					}))
				}))
			}
		}
	}
}

async fn plan(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let source = match sig.source() {
		Ok(source) => source,
		Err(e) => return error::action(&e),
	};
	let request = Request {
		source,
		credentials: Credentials {
			url: sig.imp_url.clone(),
			token: sig.imp_token.clone(),
		},
		server: sig.imp_server.clone(),
	};
	match service::preview(&s, ctx.user.id, ctx.team.id, &request).await {
		Ok(preview) => sse::patch(&previewed(&preview))
			.and(sse::signals(&json!({
				"imp_id": preview.id,
				// The credential has done its one job.
				"imp_token": "",
				"_error": "",
			})))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let source = match sig.source() {
		Ok(source) => source,
		Err(e) => return closing(sse::error(&error::message(&e))),
	};
	match service::apply(
		&s,
		ctx.user.id,
		ctx.team.id,
		source,
		&sig.imp_id,
		sig.imp_deploy,
	)
	.await
	{
		Ok(applied) => match applied.outcome {
			ImportOutcome::Done(done) => closing(
				sse::patch(&result(&plan_view::imported_inner(&done)))
					.and(sse::signals(&json!({ "imp_id": "", "_error": "" }))),
			),
			ImportOutcome::Refused(plans) => {
				closing(sse::patch(&result(&plan_view::projects_inner(&plans))))
			}
		},
		Err(e) => closing(sse::error(&error::message(&e))),
	}
}

/// The result region with `inside` in it.
fn result(inside: &Markup) -> Markup {
	html! { div id=(view::RESULT) aria-live="polite" { (inside) } }
}

/// The confirm dialog closes whatever happened; the result is on the page.
fn closing(events: sse::Events) -> Response {
	events
		.and(sse::script(&format!(
			"document.getElementById({})?.close()",
			js_string(DIALOG)
		)))
		.into_response()
}
