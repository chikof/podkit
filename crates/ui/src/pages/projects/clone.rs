//! Clone environment: the dialog in the environment tab menu, with the options and a preview
//! of what will be created, and the page that follows the clone as its job runs.
//!
//! The preview is the server's plan (`environments::clone::service::preview`), patched into
//! the dialog a moment after the name, the slug or an option changes, so the names, the
//! default domains and the conflicts shown are the ones the clone will make. The clone page
//! polls while the job runs, then shows the report: every step, what it did, what failed.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::components::{
	CHECKBOX, INPUT, LABEL, SELECT, Size, Tone, Variant, button_class, card, card_content,
	card_section, dialog, dialog_cancel, icon, log_viewer,
};
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page, form_error};
use crate::live::job_log;
use crate::sse::js_string;
use crate::status::{poll, status_badge};
use crate::{error, shell, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::environments::clone::plan::Plan;
use podkit_core::environments::clone::service::{self, Input};
use podkit_core::environments::clone::{
	CloneStatus, EnvironmentClone, Kind, Options, Secrets, StepStatus,
};
use podkit_core::error::ServerError;
use podkit_core::ids::{BackupTargetId, CloneId, EnvironmentId, ProjectId, TeamId};
use podkit_core::state::AppState;
use podkit_core::volume_backups;

pub const DIALOG_ID: &str = "environment-clone";

fn base(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}/environments")
}

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/clone",
			post(start),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/clone/preview",
			post(preview),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/clones/{clone_id}",
			get(clone_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/clones/{clone_id}/progress",
			get(progress),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/environments/clones/{clone_id}/log",
			get(log_stream),
		)
}

/// The signals the page declares for the dialog; merged into the environment page's own.
#[must_use]
pub fn signals() -> serde_json::Value {
	serde_json::json!({
		"cloneName": "",
		"cloneSlug": "",
		"cloneBlank": false,
		"cloneData": false,
		"cloneTarget": "",
		"cloneDeploy": false,
	})
}

const fn kind_icon(kind: Kind) -> &'static str {
	match kind {
		Kind::Application => "box",
		Kind::Database => "database",
		Kind::Stack => "layers",
		Kind::Model => "brain",
		Kind::Cron => "clock",
	}
}

fn count(n: usize, one: &str, many: &str) -> String {
	format!("{n} {}", if n == 1 { one } else { many })
}

// --- the dialog ---------------------------------------------------------------------------

/// What the dialog shows before there is a name to plan with, or when the plan cannot be made.
fn preview_note(text: &str) -> Markup {
	html! {
		div id="clone-preview" class="ui-clone-preview" aria-live="polite" {
			p class="ui-hint" { (text) }
		}
	}
}

/// The plan, as the dialog shows it.
fn preview_plan(plan: &Plan) -> Markup {
	let summary = [
		(plan.shared_vars > 0)
			.then(|| count(plan.shared_vars, "shared variable", "shared variables")),
		(plan.links > 0).then(|| count(plan.links, "model link", "model links")),
		(plan.networks > 0)
			.then(|| count(plan.networks, "network attachment", "network attachments")),
		(plan.blanked > 0).then(|| format!("{} emptied", count(plan.blanked, "value", "values"))),
		(plan.skipped_domains > 0).then(|| {
			count(
				plan.skipped_domains,
				"custom domain left behind",
				"custom domains left behind",
			)
		}),
	];
	let summary: Vec<String> = summary.into_iter().flatten().collect();
	html! {
		div id="clone-preview" class="ui-clone-preview" aria-live="polite" {
			@if !plan.conflicts.is_empty() {
				ul class="ui-clone-conflicts" role="alert" {
					@for c in &plan.conflicts { li { (c) } }
				}
			}
			p class="ui-hint" {
				"Creates the " strong { (plan.environment_name) } " environment"
				@if plan.items.is_empty() { ", empty." } @else { " with:" }
			}
			@if !plan.items.is_empty() {
				ul class="ui-clone-items" {
					@for item in &plan.items {
						li {
							span class="ui-clone-mark" { (icon(kind_icon(item.kind))) }
							div class="ui-clone-body" {
								span class="ui-clone-name" {
									span class="sr-only" { (item.kind.label()) " " }
									(item.new_name)
								}
								@if let Some(url) = &item.url {
									span class="ui-hint font-mono ui-clone-url" { (url) }
								}
								@if !item.data.is_empty() {
									span class="ui-hint" { "Copies: " (item.data.join(", ")) }
								}
								@if !item.notes.is_empty() {
									span class="ui-hint" { (item.notes.join("; ")) }
								}
							}
						}
					}
				}
			}
			@if !summary.is_empty() {
				p class="ui-hint" { (summary.join(", ")) "." }
			}
			@for note in &plan.notes {
				p class="ui-hint" { (note) }
			}
		}
	}
}

/// The "Clone environment" item of the environment menu.
pub fn menu_item(class: &str) -> Markup {
	let fallback = format!(
		"$_error = ''; el.commandForElement || document.getElementById({}).showModal()",
		js_string(DIALOG_ID)
	);
	html! {
		button type="button" class=(class) command="show-modal" commandfor=(DIALOG_ID)
			"data-on:click"=(fallback) {
			(icon("copy")) "Clone environment"
		}
	}
}

/// The dialog. `source` is the environment in view; its id travels in the page's `envId`.
pub async fn clone_dialog(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	source_name: &str,
) -> Markup {
	let destinations = volume_backups::service::destinations(s, ctx.user.id, ctx.team.id)
		.await
		.unwrap_or_default();
	let can_add_destination = ctx.perms.can(Action::Create, Resource::Backup);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/clone", base(ctx.team.id, project)))
	);
	let preview = format!(
		"@post({})",
		js_string(&format!("{}/clone/preview", base(ctx.team.id, project)))
	);
	dialog(
		DIALOG_ID,
		"Clone environment",
		Some(&format!(
			"Copy {source_name} into a new environment of this project: the same applications, databases, stacks and models, under their own names."
		)),
		&html! {
			form id="environment-clone-form" class="flex flex-col gap-4" "data-on:submit"=(post)
				"data-on:input__debounce.300ms"=(preview) "data-on:change__debounce.300ms"=(preview) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus
						name="clone-name" placeholder="e.g. Staging" "data-bind"="cloneName"
						"data-on:input"="$cloneSlug = $cloneName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60)";
				}
				label class=(LABEL) {
					"Slug"
					input class={ (INPUT) " font-mono" } type="text" required maxlength="60" autocomplete="off"
						name="clone-slug" placeholder="e.g. staging" "data-bind"="cloneSlug";
					span class="ui-hint" { "Added to the end of every name, slug and default domain: " code class="font-mono" { "shop-staging" } "." }
				}
				fieldset class="ui-clone-options" {
					legend { "What to copy" }
					label class="ui-check" {
						input type="checkbox" class=(CHECKBOX) name="clone-blank" "data-bind"="cloneBlank";
						span {
							span class="ui-check-title" { "Leave secret values blank" }
							span class="ui-hint" { "Variable names are kept; their values and the stored git and registry logins are not copied." }
						}
					}
					label class="ui-check" {
						input type="checkbox" class=(CHECKBOX) name="clone-data" "data-bind"="cloneData";
						span {
							span class="ui-check-title" { "Copy the data too" }
							span class="ui-hint" { "Databases are dumped and restored, volumes copied, through a backup destination. The source keeps running." }
						}
					}
					div class="flex flex-col gap-1" "data-show"="$cloneData" style="display: none" {
						label class=(LABEL) {
							"Backup destination"
							select class=(SELECT) name="clone-target" "data-bind"="cloneTarget" {
								button { selectedcontent {} }
								option value="" { "Choose a destination" }
								@for (target, name) in &destinations {
									option value=(target.0) { (name) }
								}
							}
						}
						@if destinations.is_empty() {
							span class="ui-hint" {
								"There is no destination yet. "
								@if can_add_destination {
									a class="ui-link" href={ "/teams/" (ctx.team.id) "/backup-targets" } { "Add an S3 destination" }
									" first."
								}
							}
						}
					}
					label class="ui-check" {
						input type="checkbox" class=(CHECKBOX) name="clone-deploy" "data-bind"="cloneDeploy";
						span {
							span class="ui-check-title" { "Deploy after cloning" }
							span class="ui-hint" { "Deploy the applications and stacks and start the models. Otherwise nothing runs until you do." }
						}
					}
				}
				(preview_note("Name the environment to see what will be created."))
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="submit" form="environment-clone-form" class=(button_class(Variant::Default, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Clone environment" }
		},
	)
}

// --- the requests -----------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct CloneSignals {
	#[serde(rename = "envId")]
	env_id: String,
	#[serde(rename = "cloneName")]
	name: String,
	#[serde(rename = "cloneSlug")]
	slug: String,
	#[serde(rename = "cloneBlank")]
	blank: bool,
	#[serde(rename = "cloneData")]
	data: bool,
	#[serde(rename = "cloneTarget")]
	target: String,
	#[serde(rename = "cloneDeploy")]
	deploy: bool,
}

impl CloneSignals {
	fn source(&self) -> Option<EnvironmentId> {
		self.env_id.parse::<i64>().ok().map(EnvironmentId)
	}

	fn input(&self) -> Input {
		Input {
			name: self.name.clone(),
			slug: self.slug.clone(),
			options: Options {
				secrets: if self.blank {
					Secrets::Blank
				} else {
					Secrets::Copy
				},
				data: self.data,
				backup_target_id: self.target.parse::<i64>().ok().map(BackupTargetId),
				deploy: self.deploy,
			},
		}
	}
}

async fn preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<CloneSignals>,
) -> Response {
	let Some(source) = sig.source() else {
		return sse::patch(&preview_note("Pick an environment to clone.")).into_response();
	};
	if sig.name.trim().is_empty() || sig.slug.is_empty() {
		return sse::patch(&preview_note(
			"Name the environment to see what will be created.",
		))
		.into_response();
	}
	match service::preview(&s, ctx.user.id, ctx.team.id, project, source, &sig.input()).await {
		Ok(plan) => sse::patch(&preview_plan(&plan)).into_response(),
		// A name that is not valid yet is the person still typing, not a failure.
		Err(ServerError::Validation(m)) => sse::patch(&preview_note(&m)).into_response(),
		Err(e) => sse::patch(&preview_note(&error::message(&e))).into_response(),
	}
}

async fn start(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: Cookies,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<CloneSignals>,
) -> Response {
	let Some(source) = sig.source() else {
		return sse::error("Pick an environment to clone.").into_response();
	};
	match service::clone(&s, ctx.user.id, ctx.team.id, project, source, sig.input()).await {
		Ok(clone) => {
			shell::flash(
				&cookies,
				Tone::Success,
				&format!("Cloning {} into {}.", clone.source_name, clone.name),
			);
			sse::redirect(&format!(
				"{}/clones/{}",
				base(ctx.team.id, project),
				clone.id
			))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

// --- the clone page -----------------------------------------------------------------------------

fn step_mark(status: StepStatus) -> Markup {
	let (name, label) = match status {
		StepStatus::Pending => ("circle", "Waiting"),
		StepStatus::Running => ("loader-circle", "Running"),
		StepStatus::Ok => ("circle-check", "Done"),
		StepStatus::Failed => ("circle-x", "Failed"),
		StepStatus::Skipped => ("minus", "Skipped"),
	};
	html! {
		span class="ui-clone-mark" data-step=(match status {
			StepStatus::Pending => "pending",
			StepStatus::Running => "running",
			StepStatus::Ok => "ok",
			StepStatus::Failed => "failed",
			StepStatus::Skipped => "skipped",
		}) {
			(icon(name))
			span class="sr-only" { (label) ": " }
		}
	}
}

/// The status and steps of the clone: the region the page polls while the job runs.
fn progress_region(ctx: &TeamCtx, project: ProjectId, clone: &EnvironmentClone) -> Markup {
	let url = format!(
		"{}/clones/{}/progress",
		base(ctx.team.id, project),
		clone.id
	);
	let working = !clone.status.is_final();
	html! {
		div id="clone-progress" class="ui-clone-progress" {
			(poll(&url, working))
			div class="flex flex-wrap items-center gap-3" {
				(status_badge(clone.status))
				@if let Some(env) = clone.environment_id {
					a class="ui-link" href={ "/teams/" (ctx.team.id) "/projects/" (project) "?environment=" (env) } {
						"Open " (clone.name)
					}
				}
			}
			@if clone.status == CloneStatus::Partial || clone.status == CloneStatus::Failed {
				p class="ui-hint" {
					"The environment and its resources exist. What failed below can be done again from the resource's own page."
				}
			}
			@if clone.report.0.is_empty() {
				p class="ui-hint" { "There was nothing to start or copy: the environment was made empty." }
			} @else {
				ol class="ui-clone-steps" {
					@for step in &clone.report.0 {
						li {
							(step_mark(step.status))
							div class="ui-clone-body" {
								span class="ui-clone-name" { (step.label) }
								@if let Some(detail) = &step.detail {
									span class="ui-hint" { (detail) }
								}
							}
						}
					}
				}
			}
		}
	}
}

fn crumbs<'a>(ctx: &TeamCtx, project_name: &'a str, project: ProjectId) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project_name,
			Some(format!("/teams/{}/projects/{project}", ctx.team.id)),
		),
		("Clone", None),
	]
}

async fn clone_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, CloneId)>,
) -> Result<Markup, Response> {
	let clone = service::get(&s.db, ctx.user.id, ctx.team.id, project, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let project_name =
		podkit_core::projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
			.await
			.map_err(|e| error::page(&ctx, &e))?
			.name;
	let log_url = format!("{}/clones/{}/log", base(ctx.team.id, project), clone.id);
	let body = html! {
		div class="ui-wide ui-res-page" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Clone environment" }
					h1 { (clone.source_name) " into " (clone.name) }
					p class="ui-lede" { "The configuration is copied. Databases start, data is copied and resources are deployed as the options asked." }
				}
			}
			(card(&html! {
				(card_section("Progress", None, None))
				(card_content(&progress_region(&ctx, project, &clone)))
			}))
			@if clone.job_id.is_some() {
				(card(&html! {
					(card_section("Log", None, None))
					(card_content(&log_viewer("clone-log", "Clone log", &log_url)))
				}))
			}
		}
	};
	Ok(app_page(
		&ctx,
		"Clone environment",
		&crumbs(&ctx, &project_name, project),
		&body,
	))
}

async fn progress(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, CloneId)>,
) -> Response {
	match service::get(&s.db, ctx.user.id, ctx.team.id, project, id).await {
		Ok(clone) => sse::patch(&progress_region(&ctx, project, &clone)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn log_stream(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, CloneId)>,
) -> Response {
	match service::get(&s.db, ctx.user.id, ctx.team.id, project, id).await {
		Ok(clone) => job_log(&s, clone.job_id, "clone-log"),
		Err(e) => log_failure(&e),
	}
}

/// A log stream that could not start: say so inside the log and stop the "Live" indicator.
fn log_failure(e: &ServerError) -> Response {
	let line = html! {
		div class="break-all whitespace-pre-wrap text-muted-foreground" { (error::message(e)) }
	};
	sse::patch_into("#clone-log", ElementPatchMode::Append, &line).into_response()
}
