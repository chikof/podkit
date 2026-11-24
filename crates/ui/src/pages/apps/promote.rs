//! Promote: deploy the image this application runs to the matching application of another
//! environment of its project, without building it again (`deployments::promote`).
//!
//! The header's menu opens a dialog and asks for its body, which says what would be deployed
//! (the running deployment, its commit and image) and, for each other environment, which
//! application would get it, or why none would. Each destination has its own button; the
//! post names the source deployment the dialog showed, so a release that changed in between is
//! refused instead of shipped unseen.

use std::fmt::Write as _;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};

use super::app_url;
use super::header::{deployment_url, fail};
use crate::components::{Size, Variant, button_class, dialog, icon};
use crate::extract::TeamCtx;
use crate::sse;
use crate::sse::js_string;
use podkit_core::applications::SourceKind;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::deployments::promote::{self, Destination, Plan};
use podkit_core::ids::{ApplicationId, DeploymentId, EnvironmentId, ProjectId, TeamId};
use podkit_core::state::AppState;

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

/// The id of the dialog, and of the element its body is patched into.
pub const DIALOG: &str = "promote-dialog";
const BODY: &str = "promote-body";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{APP}{path}");
	Router::new()
		.route(&at("/promote"), get(body))
		.route(&at("/promote/{environment_id}"), post(run))
}

/// What the menu entry does: clear the error line, ask for a fresh body and open the dialog.
#[must_use]
pub fn open_action(base: &str) -> String {
	format!(
		"$_action_error = ''; @get({}); el.commandForElement || document.getElementById({}).showModal()",
		js_string(&format!("{base}/promote")),
		js_string(DIALOG)
	)
}

/// The dialog, with a placeholder body until the menu entry asks for the real one. Shown to
/// someone who may deploy.
pub fn dialog_for(ctx: &TeamCtx) -> Markup {
	if !ctx.perms.can(Action::Create, Resource::Deployment) {
		return html! {};
	}
	dialog(
		DIALOG,
		"Promote to another environment",
		Some(
			"Nothing is rebuilt: the image this application runs is deployed to the same application in the environment you pick, with that environment's variables, domains and health check.",
		),
		&html! {
			div id=(BODY) class="flex flex-col gap-3" {
				p class="text-sm text-muted-foreground" { "Loading\u{2026}" }
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
		},
		&html! {
			form method="dialog" { button class=(button_class(Variant::Outline, Size::Md)) autofocus { "Cancel" } }
		},
	)
}

fn short(sha: &str) -> &str {
	sha.get(..7).unwrap_or(sha)
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div {
			dt { (label) }
			dd { (value) }
		}
	}
}

/// What one destination would get, in a line: the application, its server, and what happens
/// to the image on the way.
fn destination_note(plan: &Plan, d: &Destination) -> String {
	let image = plan.source.as_ref().and_then(|s| s.image_tag.as_deref());
	match &d.matched {
		Err(why) => why.clone(),
		Ok(m) => {
			let mut note = format!("{} on {}", m.application.slug, m.server.name);
			if m.runs(image) {
				note.push_str(". Already runs this image; promoting starts it again.");
			} else if let Some(running) = &m.running {
				let _ = write!(note, ". Replaces deployment #{}.", running.id);
			} else {
				note.push_str(". Nothing runs there yet.");
			}
			if m.moves_image {
				let _ = write!(
					note,
					" The image is copied from {} to {} first.",
					plan.server.name, m.server.name
				);
			}
			note
		}
	}
}

/// The dialog's body: the source, then one row per other environment.
fn body_markup(ctx: &TeamCtx, plan: &Plan) -> Markup {
	let base = app_url(
		ctx.team.id,
		plan.application.project_id,
		plan.application.id,
	);
	let app = &plan.application;
	html! {
		div id=(BODY) class="flex flex-col gap-4" {
			@match &plan.source {
				None => {
					p class="text-sm" {
						(app.name) " is not running in " (plan.environment.name)
						", so there is no image to promote. Deploy it first."
					}
				}
				Some(source) => {
					dl class="ui-facts ui-facts-row" aria-label="What would be deployed" {
						(fact("From", &html! { (plan.environment.name) }))
						(fact("Deployment", &html! {
							a href=(deployment_url(&base, source.id)) { "#" (source.id) }
						}))
						@if app.source_kind == SourceKind::Git {
							(fact("Commit", &html! {
								@if let Some(sha) = &source.commit_sha { code { (short(sha)) } } @else { "\u{2014}" }
							}))
						}
						(fact("Image", &html! {
							code class="break-all" { (source.image_tag.as_deref().unwrap_or_default()) }
						}))
					}
					@if plan.destinations.is_empty() {
						p class="text-sm text-muted-foreground" {
							"This project has only one environment. Add another to the project to promote between them."
						}
					} @else {
						ul class="flex flex-col gap-2" aria-label="Environments to promote to" {
							@for d in &plan.destinations {
								(destination_row(&base, plan, d, source.id))
							}
						}
					}
				}
			}
		}
	}
}

fn destination_row(base: &str, plan: &Plan, d: &Destination, source: DeploymentId) -> Markup {
	let post = format!(
		"@post({})",
		js_string(&format!(
			"{base}/promote/{}?deployment={source}",
			d.environment.id
		))
	);
	html! {
		li class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border px-4 py-3" {
			div class="flex min-w-0 flex-col gap-0.5" {
				span class="text-sm font-medium" { (d.environment.name) }
				span class={ "text-xs " @if d.matched.is_ok() { "text-muted-foreground" } @else { "text-destructive" } } {
					(destination_note(plan, d))
				}
			}
			@if d.matched.is_ok() {
				button type="button" class=(button_class(Variant::Default, Size::Sm)) "data-on:click"=(post)
					"data-indicator:_acting" "data-attr:disabled"="$_acting" {
					(icon("arrow-up-right")) "Promote to " (d.environment.name)
				}
			}
		}
	}
}

/// The dialog's body, asked for each time it opens, so it shows what runs now.
async fn body(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match promote::plan(&s.db, ctx.user.id, ctx.team.id, project, app).await {
		Ok(plan) => sse::patch(&body_markup(&ctx, &plan))
			.and(super::header::action_error(""))
			.into_response(),
		Err(e) => fail(&e),
	}
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Source {
	deployment: Option<DeploymentId>,
}

/// Promotes, then opens the new deployment on the target application's page.
async fn run(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, environment)): Path<(TeamId, ProjectId, ApplicationId, EnvironmentId)>,
	Query(source): Query<Source>,
) -> Response {
	match promote::promote(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		app,
		environment,
		source.deployment,
	)
	.await
	{
		Ok(done) => sse::redirect(&deployment_url(
			&app_url(ctx.team.id, project, done.application.id),
			done.deployment.id,
		))
		.into_response(),
		Err(e) => fail(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_menu_entry_asks_for_the_body_and_opens_the_dialog() {
		let js = open_action("/teams/1/projects/2/apps/3");
		assert!(
			js.contains("@get('/teams/1/projects/2/apps/3/promote')"),
			"{js}"
		);
		assert!(js.contains("promote-dialog"), "{js}");
	}
}
