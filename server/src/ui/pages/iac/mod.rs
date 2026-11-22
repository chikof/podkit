//! Infrastructure as code in the web UI: a project's page to export it, to plan a file against
//! it and to apply the file; and the team-level import that makes new projects from a file.
//!
//! A Configure surface: the person is here to bring a file and see what it would do before
//! it does it. The page is a form, then the plan under it as a diff, then one confirmed
//! button. Every action goes through [`crate::iac::service`], which authorizes and scopes it
//! exactly as the JSON API does, so a forged POST is refused the same way.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::auth::permission::{Action, Resource};
use crate::error::{AppResult, ServerError};
use crate::iac::objects::Secrets;
use crate::iac::service::{self, ImportOutcome, Input, Outcome};
use crate::ids::{ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, CHECKBOX, LABEL, Size, Variant, button_class, card, card_content, card_section, dialog,
	icon,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub(super) mod plan_view;
#[cfg(test)]
mod tests;

/// The largest file taken from the form: a podkit.toml is text of a few kilobytes.
const MAX_FILE: usize = 2 * 1024 * 1024;

const APPLY_DIALOG: &str = "iac-apply";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/iac",
			get(project_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/iac/export",
			get(download),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/iac/plan",
			post(project_plan),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/iac/apply",
			post(project_apply),
		)
		.route("/teams/{team_id}/import", get(import_page))
		.route("/teams/{team_id}/import/plan", post(import_plan))
		.route("/teams/{team_id}/import/apply", post(import_apply))
}

fn project_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}")
}

fn iac_url(team: TeamId, project: ProjectId) -> String {
	format!("{}/iac", project_url(team, project))
}

/// What the form sends: the files as Datastar reads them (name, base64 contents), and the
/// two options.
#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the Datastar signals of the form"
)]
struct Signals {
	iac_file: Vec<Upload>,
	iac_secrets: Vec<Upload>,
	iac_prune: bool,
	iac_deploy: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Upload {
	contents: String,
}

fn text_of(files: &[Upload], what: &str) -> AppResult<Option<String>> {
	let Some(file) = files.first() else {
		return Ok(None);
	};
	let bytes = base64::engine::general_purpose::STANDARD
		.decode(file.contents.trim())
		.map_err(|_| ServerError::Validation(format!("The {what} could not be read.")))?;
	if bytes.len() > MAX_FILE {
		return Err(ServerError::Validation(format!("The {what} is too large.")));
	}
	String::from_utf8(bytes)
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("The {what} is not UTF-8 text.")))
}

impl Signals {
	fn input(&self) -> AppResult<Input> {
		let text = text_of(&self.iac_file, "file")?
			.ok_or_else(|| ServerError::Validation("Choose a podkit.toml first.".to_owned()))?;
		let secrets = match text_of(&self.iac_secrets, "secrets file")? {
			Some(t) => Secrets::parse(&t).map_err(ServerError::Validation)?,
			None => Secrets::default(),
		};
		Ok(Input {
			text,
			secrets,
			prune: self.iac_prune,
			deploy: self.iac_deploy,
		})
	}
}

fn can_apply(ctx: &TeamCtx) -> bool {
	ctx.perms.can(Action::Update, Resource::Application)
		|| ctx.perms.can(Action::Create, Resource::Application)
}

/// The signals of the form; underscore ones stay in the browser.
fn signals() -> String {
	json!({
		"iac_file": [], "iac_secrets": [], "iac_prune": false, "iac_deploy": false,
		"_error": "", "_busy": false, "_applying": false,
	})
	.to_string()
}

/// The two file pickers and the options: the same on both pages.
fn form_fields(prune_hint: &str) -> Markup {
	html! {
		label class=(LABEL) for="iac-file" {
			"File"
			input id="iac-file" name="file" class="ui-field" type="file" required
				accept=".toml,application/toml,text/plain" aria-describedby="iac-file-help"
				"data-bind:iac_file";
			span id="iac-file-help" class="ui-hint" { "A podkit.toml, for instance one you exported and edited." }
		}
		label class=(LABEL) for="iac-secrets" {
			"Secrets file (optional)"
			input id="iac-secrets" name="secrets" class="ui-field" type="file"
				accept=".toml,application/toml,text/plain" aria-describedby="iac-secrets-help"
				"data-bind:iac_secrets";
			span id="iac-secrets-help" class="ui-hint" {
				"A flat TOML table of NAME = \"value\" for the ${secret:NAME} placeholders. It is read once and never stored."
			}
		}
		fieldset class="ui-iac-options" {
			legend { "Options" }
			div class="ui-check" {
				input id="iac-prune" name="prune" type="checkbox" class=(CHECKBOX) aria-describedby="iac-prune-help"
					"data-bind:iac_prune";
				label for="iac-prune" class="cursor-pointer" { "Prune" }
			}
			p id="iac-prune-help" class="ui-hint pl-6" { (prune_hint) }
			div class="ui-check" {
				input id="iac-deploy" name="deploy" type="checkbox" class=(CHECKBOX) aria-describedby="iac-deploy-help"
					"data-bind:iac_deploy";
				label for="iac-deploy" class="cursor-pointer" { "Deploy after applying" }
			}
			p id="iac-deploy-help" class="ui-hint pl-6" {
				"Queue a deploy of every application and stack the apply creates or changes."
			}
		}
	}
}

fn apply_button() -> Markup {
	html! {
		button type="button" class=(BUTTON) command="show-modal" commandfor=(APPLY_DIALOG)
			aria-haspopup="dialog"
			"data-on:click"=(format!("el.commandForElement || document.getElementById({}).showModal()", js_string(APPLY_DIALOG))) {
			(icon("check")) "Apply\u{2026}"
		}
	}
}

fn apply_dialog(post_to: &str, what: &str) -> Markup {
	let post = format!("@post({})", js_string(post_to));
	dialog(
		APPLY_DIALOG,
		&format!("Apply this file to {what}?"),
		Some(
			"The file is planned again against what exists now, then applied. A change that fails stops the rest.",
		),
		&html! {
			div class="flex flex-col gap-3" {
				p class="text-sm" { "Changes go through the same checks as the pages do, so anything you may not do is refused before it starts." }
				p class="text-sm text-destructive" "data-show"="$iac_prune" style="display: none" {
					"Prune is on: what the file leaves out is deleted."
				}
				p class="text-sm" "data-show"="$iac_deploy" style="display: none" {
					"Deploy is on: applications and stacks that change are deployed."
				}
			}
		},
		&html! {
			form method="dialog" {
				button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Not yet" }
			}
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post)
				"data-indicator:_applying" "data-attr:disabled"="$_applying" { "Apply" }
		},
	)
}

fn crumbs<'a>(ctx: &TeamCtx, project: Option<(&'a str, String)>) -> Vec<Crumb<'a>> {
	let mut crumbs = vec![("Projects", Some(format!("/teams/{}/projects", ctx.team.id)))];
	if let Some((name, url)) = project {
		crumbs.push((name, Some(url)));
		crumbs.push(("Infrastructure as code", None));
	} else {
		crumbs.push(("Import", None));
	}
	crumbs
}

// ----- a project

async fn project_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	let project = service::project(&s, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = iac_url(ctx.team.id, project.id);
	let plan_post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/plan"))
	);
	let body = html! {
		div class="ui-page-body" data-signals=(signals()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { (project.name) }
					h1 { "Infrastructure as code" }
					p class="ui-lede" {
						"This project as a podkit.toml you can keep in git. Plan a file to see what it would change, then apply it."
					}
				}
				div class="ui-page-actions" {
					a class=(button_class(Variant::Outline, Size::Md)) href=(format!("{base}/export")) download="podkit.toml" {
						(icon("download")) "Export podkit.toml"
					}
				}
			}
			(card(&html! {
				(card_section("Plan a file", Some("Nothing changes until you apply."), None))
				(card_content(&html! {
					form id="iac-form" class="flex flex-col gap-4" "data-on:submit"=(plan_post) "data-indicator:_busy" {
						(form_fields("Delete what the file leaves out. Without it nothing is ever deleted."))
						(form_error())
						div class="flex justify-end" {
							button type="submit" class=(BUTTON) "data-attr:disabled"="$_busy" {
								(icon("file-text")) "Plan"
							}
						}
					}
				}))
			}))
			(plan_view::empty())
			@if can_apply(&ctx) {
				(apply_dialog(&format!("{base}/apply"), "this project"))
			}
		}
	};
	let name = project.name.clone();
	let url = project_url(ctx.team.id, project.id);
	Ok(app_page(
		&ctx,
		"Infrastructure as code",
		&crumbs(&ctx, Some((name.as_str(), url))),
		&body,
	))
}

async fn download(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Response {
	match service::export(&s, ctx.user.id, ctx.team.id, project).await {
		Ok(text) => (
			[
				(header::CONTENT_TYPE, "application/toml; charset=utf-8"),
				(
					header::CONTENT_DISPOSITION,
					"attachment; filename=\"podkit.toml\"",
				),
			],
			text,
		)
			.into_response(),
		Err(e) => error::page(&ctx, &e),
	}
}

async fn project_plan(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let input = match sig.input() {
		Ok(input) => input,
		Err(e) => return error::action(&e),
	};
	match service::plan(&s, ctx.user.id, ctx.team.id, project, &input).await {
		Ok(plan) => {
			let apply = can_apply(&ctx).then(apply_button);
			sse::patch(&plan_view::planned(&plan, apply.as_ref()))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn project_apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let input = match sig.input() {
		Ok(input) => input,
		Err(e) => return closing(sse::error(&error::message(&e))),
	};
	match service::apply(&s, ctx.user.id, ctx.team.id, project, &input).await {
		Ok(Outcome::Done(done)) => closing(sse::patch(&plan_view::applied(&done))),
		Ok(Outcome::Refused(plan)) => closing(sse::patch(&plan_view::planned(&plan, None))),
		Err(e) => closing(sse::error(&error::message(&e))),
	}
}

/// The confirm dialog closes whatever happened; the result is under the form.
fn closing(events: sse::Events) -> Response {
	events
		.and(sse::script(&format!(
			"document.getElementById({})?.close()",
			js_string(APPLY_DIALOG)
		)))
		.into_response()
}

// ----- the team import

async fn import_page(State(_s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	if !ctx.perms.can(Action::Create, Resource::Project) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let base = format!("/teams/{}/import", ctx.team.id);
	let plan_post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/plan"))
	);
	let body = html! {
		div class="ui-page-body" data-signals=(signals()) {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Projects" }
					h1 { "Import projects" }
					p class="ui-lede" {
						"Make new projects from a podkit.toml. A project whose slug already exists is not touched: open it and apply the file there."
					}
				}
			}
			(card(&html! {
				(card_section("Plan an import", Some("Nothing is made until you import."), None))
				(card_content(&html! {
					form id="iac-form" class="flex flex-col gap-4" "data-on:submit"=(plan_post) "data-indicator:_busy" {
						(form_fields("Not used for new projects. An import still removes the empty production environment a new project starts with, if the file has none."))
						(form_error())
						div class="flex justify-end" {
							button type="submit" class=(BUTTON) "data-attr:disabled"="$_busy" {
								(icon("upload")) "Plan"
							}
						}
					}
				}))
			}))
			(plan_view::empty())
			(apply_dialog(&format!("{base}/apply"), "this team"))
		}
	};
	Ok(app_page(
		&ctx,
		"Import projects",
		&crumbs(&ctx, None),
		&body,
	))
}

async fn import_plan(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let input = match sig.input() {
		Ok(input) => input,
		Err(e) => return error::action(&e),
	};
	match service::import_plan(&s, ctx.user.id, ctx.team.id, &input).await {
		Ok(plans) => {
			let apply = apply_button();
			sse::patch(&plan_view::projects(&plans, Some(&apply)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn import_apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let input = match sig.input() {
		Ok(input) => input,
		Err(e) => return closing(sse::error(&error::message(&e))),
	};
	match service::import_apply(&s, ctx.user.id, ctx.team.id, &input).await {
		Ok(ImportOutcome::Done(done)) => closing(sse::patch(&plan_view::imported(&done))),
		Ok(ImportOutcome::Refused(plans)) => {
			closing(sse::patch(&plan_view::projects(&plans, None)))
		}
		Err(e) => closing(sse::error(&error::message(&e))),
	}
}
