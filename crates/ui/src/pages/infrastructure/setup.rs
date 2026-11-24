//! The guided setup: a stepper over the six steps and, below it, the open step with the
//! live state of what it is about. Every state is read from what the team has (see
//! `infrastructure::setup`), so a step turns "Done" by itself once the work is done
//! anywhere else in podkit. Only skipping an optional part and dismissing the checklist
//! are stored.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde::Deserialize;

use super::{crumbs, overview_url, setup_url};
use crate::components::{
	BUTTON, BUTTON_OUTLINE, BannerKind, Current, Size, Tone, Variant, badge, banner, button_class,
	card, card_content, card_footer, card_section, empty_state, icon, link_button, stepper,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::app_page;
use crate::sse::{self, js_string};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::TeamId;
use podkit_core::infrastructure::service::{self, SetupView};
use podkit_core::infrastructure::setup::{Check, Item, State as StepState, Step, StepView};
use podkit_core::state::AppState;

fn step_url(team: TeamId, step: Step) -> String {
	format!("{}?step={}", setup_url(team), step.slug())
}

fn tone(state: StepState) -> Tone {
	match state {
		StepState::Done => Tone::Success,
		StepState::Todo | StepState::Skipped => Tone::Neutral,
	}
}

/// The mark in front of a line: a check when done, a dash when skipped, an empty ring when
/// there is something to do. The word is in the text, so color is never the only signal.
fn mark(state: StepState) -> Markup {
	html! {
		span class="ui-todo-mark" data-state=(match state {
			StepState::Done => "done",
			StepState::Todo => "todo",
			StepState::Skipped => "skipped",
		}) aria-hidden="true" {
			@match state {
				StepState::Done => (icon("check")),
				StepState::Skipped => (icon("minus")),
				StepState::Todo => {},
			}
		}
	}
}

/// The checklist the overview shows: one row per step with its state, each a link into the
/// step, and the two ways out (continue, dismiss).
pub(super) fn checklist(ctx: &TeamCtx, view: &SetupView) -> Markup {
	let team = ctx.team.id;
	let can_update = ctx.perms.can(Action::Update, Resource::Team);
	let next = view
		.current
		.map_or_else(|| setup_url(team), |s| step_url(team, s));
	let dismiss = format!(
		"@post({})",
		js_string(&format!("{}/dismiss", setup_url(team)))
	);
	card(&html! {
		(card_section(
			"Set up your infrastructure",
			Some(&format!("{} of {} steps done. Each one is checked against what you really have.", view.done_count(), view.steps.len())),
			None,
		))
		(card_content(&html! {
			ol class="ui-todo" aria-label="Setup steps" {
				@for step in &view.steps {
					li class="ui-todo-row" {
						(mark(step.state))
						div class="ui-todo-main" {
							a class="ui-todo-name" href=(step_url(team, step.step)) { (step.step.label()) }
							span class="ui-todo-detail" { (step.checks.first().map_or("", |c| c.detail.as_str())) }
						}
						span class="ui-todo-state" { (badge(tone(step.state), step.state.label())) }
					}
				}
			}
		}))
		(card_footer(&html! {
			@if can_update {
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(dismiss) { "Dismiss" }
			}
			(link_button(Variant::Default, Size::Md, &next, &html! { "Continue setup" }))
		}))
	})
}

/// Where a line of a step leads, and what the link says. A link that creates something is
/// offered only to someone who may create it; everyone else gets the page that lists it.
fn action(ctx: &TeamCtx, step: Step, check: &Check) -> Option<(&'static str, String)> {
	let team = ctx.team.id;
	let can = |a, r| ctx.perms.can(a, r);
	match (step, check.item) {
		(Step::Server, _) => Some(if can(Action::Create, Resource::Server) {
			("Add a server", format!("/teams/{team}/servers"))
		} else {
			("Open servers", format!("/teams/{team}/servers"))
		}),
		(Step::Networks, _) => Some((
			"Open networks",
			format!("/teams/{team}/infrastructure/networks"),
		)),
		(Step::Access, Some(Item::SshKey)) => Some(if can(Action::Create, Resource::SshKey) {
			("Add a key", format!("/teams/{team}/keys"))
		} else {
			("Open keys", format!("/teams/{team}/keys"))
		}),
		(Step::Access, Some(Item::GitProvider)) => {
			Some(if can(Action::Create, Resource::GitProvider) {
				("Connect a provider", format!("/teams/{team}/git-providers"))
			} else {
				("Open git providers", format!("/teams/{team}/git-providers"))
			})
		}
		(Step::Access, Some(Item::Registry)) => Some(if can(Action::Create, Resource::Registry) {
			("Add a registry", format!("/teams/{team}/registries"))
		} else {
			("Open registries", format!("/teams/{team}/registries"))
		}),
		(Step::Security, _) => Some(("Open security", format!("/teams/{team}/security"))),
		(Step::Project, _) => Some(if can(Action::Create, Resource::Project) {
			("Create a project", format!("/teams/{team}/projects"))
		} else {
			("Open projects", format!("/teams/{team}/projects"))
		}),
		_ => None,
	}
}

fn check_row(ctx: &TeamCtx, step: Step, check: &Check) -> Markup {
	let team = ctx.team.id;
	let can_update = ctx.perms.can(Action::Update, Resource::Team);
	let link = action(ctx, step, check);
	html! {
		li class="ui-todo-row" {
			(mark(check.state))
			div class="ui-todo-main" {
				span class="ui-todo-name" { (check.label) }
				span class="ui-todo-detail" { (check.detail) }
			}
			div class="ui-todo-act" {
				span class="ui-visually-hidden" { (check.state.label()) }
				@if check.state != StepState::Done {
					@if let Some(item) = check.item {
						@if can_update {
							@let verb = if check.state == StepState::Skipped { "unskip" } else { "skip" };
							@let url = format!("{}/{verb}/{}", setup_url(team), item.slug());
							button type="button" class=(button_class(Variant::Ghost, Size::Sm))
								"data-on:click"={ "@post(" (js_string(&url)) ")" } {
								@if check.state == StepState::Skipped { "Undo skip" } @else { "Skip" }
								span class="ui-visually-hidden" { " " (item.label()) }
							}
						}
					}
				}
				@if let Some((label, href)) = link {
					a class=(button_class(Variant::Outline, Size::Sm)) href=(href) { (label) }
				}
			}
		}
	}
}

/// What the step is about, in a sentence or two, before its lines.
fn intro(step: Step) -> &'static str {
	match step {
		Step::Server => {
			"podkit runs containers on servers. The one it runs on is added for you and checked when it starts. Add a remote one with an SSH key from the vault."
		}
		Step::Proxy => {
			"Traefik routes web traffic to your applications by hostname. It starts on a server together with the first application you run there."
		}
		Step::Networks => {
			"Every environment gets its own private network on each server, so production and staging never see each other. Resources on a network reach each other by name. Make a shared network when two environments, or two projects, need to talk."
		}
		Step::Access => {
			"A key lets podkit log in to servers and clone private repositories. A git provider lists your repositories and deploys on push. A registry keeps the login for private images. Each one is optional."
		}
		Step::Security => {
			"Every image is scanned when it deploys and again each day. A finding warns; it never blocks a deploy. The first scan downloads the vulnerability database."
		}
		Step::Project => {
			"A project groups what belongs together and starts with a production environment. Then pick what to run: an application from git, an image, a database, a model or a template."
		}
	}
}

fn proxy_facts(view: &SetupView) -> Markup {
	let p = &view.proxy;
	html! {
		dl class="ui-fact-list" {
			div class="ui-fact" { dt { "HTTP port" } dd { code { ":" (p.http_port) } } }
			div class="ui-fact" {
				dt { "HTTPS port" }
				dd {
					@if p.acme_email.is_some() { code { ":" (p.https_port) } } @else { "Closed: no ACME email is set" }
				}
			}
			div class="ui-fact" {
				dt { "ACME email" }
				dd {
					@if let Some(email) = &p.acme_email { (email) } @else {
						"Not set. Custom domains are served over HTTP only. Set "
						code { "ACME_EMAIL" }
						" on the podkit host to get Let's Encrypt certificates."
					}
				}
			}
		}
	}
}

fn step_card(ctx: &TeamCtx, view: &SetupView, sv: &StepView) -> Markup {
	let heading = html! {
		span class="ui-todo-title" {
			(sv.step.label())
			(badge(tone(sv.state), sv.state.label()))
		}
	};
	card(&html! {
		div class="ui-card-h" {
			div {
				h2 class="ui-card-title" { (heading) }
				p class="ui-card-sub" { (intro(sv.step)) }
			}
		}
		(card_content(&html! {
			@if sv.step == Step::Proxy { (proxy_facts(view)) }
			ul class="ui-todo" aria-label={ (sv.step.label()) " status" } {
				@for check in &sv.checks { (check_row(ctx, sv.step, check)) }
			}
		}))
	})
}

#[derive(Deserialize)]
pub(super) struct StepQuery {
	step: Option<String>,
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(q): Query<StepQuery>,
) -> Result<Markup, Response> {
	let view = service::setup(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let team = ctx.team.id;
	let can_update = ctx.perms.can(Action::Update, Resource::Team);
	// A step the person has no part in is a 404 like any page that is not theirs.
	let asked = match q.step.as_deref() {
		Some(slug) => {
			let step = Step::from_slug(slug).ok_or(ServerError::NotFound);
			Some(
				step.and_then(|st| service::find_step(&view, st).map(|v| v.step))
					.map_err(|e| error::page(&ctx, &e))?,
			)
		}
		None => None,
	};
	let at = asked
		.or(view.current)
		.and_then(|st| view.steps.iter().position(|v| v.step == st));
	let body = html! {
		div class="ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Set up your infrastructure" }
					p class="ui-lede" { "Six steps from a bare host to a first deploy. Skip what you do not need; podkit marks a step done when it finds the work." }
				}
				div class="ui-page-actions" {
					a class=(button_class(Variant::Outline, Size::Md)) href=(overview_url(team)) { "Back to overview" }
					@if can_update && !view.steps.is_empty() {
						@if view.progress.dismissed {
							button type="button" class=(BUTTON_OUTLINE)
								"data-on:click"={ "@post(" (js_string(&format!("{}/reopen", setup_url(team)))) ")" } {
								"Show on the overview again"
							}
						} @else {
							button type="button" class=(BUTTON_OUTLINE)
								"data-on:click"={ "@post(" (js_string(&format!("{}/dismiss", setup_url(team)))) ")" } {
								"Dismiss"
							}
						}
					}
				}
			}
			@if view.steps.is_empty() {
				(empty_state(
					"circle-check",
					"Nothing to set up",
					Some("Your role has no part in the setup. Ask the team owner for access to servers or projects."),
					None,
				))
			} @else if let Some(at) = at {
				@let labels: Vec<&str> = view.steps.iter().map(|v| v.step.label()).collect();
				(stepper("Setup steps", &labels, &Current::At(at + 1)))
				@if view.complete {
					(banner(BannerKind::Success, Some("All set."), "Every step is done or skipped.", None))
				}
				(step_card(&ctx, &view, &view.steps[at]))
				nav class="ui-setup-nav" aria-label="Setup navigation" {
					@if at > 0 {
						a class=(button_class(Variant::Outline, Size::Md)) href=(step_url(team, view.steps[at - 1].step)) {
							(icon("arrow-left")) "Back"
						}
					} @else { span {} }
					@if let Some(next) = view.steps.get(at + 1) {
						a class=(BUTTON) href=(step_url(team, next.step)) { "Next: " (next.step.label()) (icon("arrow-right")) }
					} @else {
						a class=(BUTTON) href=(overview_url(team)) { "Finish" }
					}
				}
			}
		}
	};
	Ok(app_page(
		&ctx,
		"Set up your infrastructure",
		&[crumbs(&ctx).remove(0), ("Setup", Some(setup_url(team)))],
		&body,
	))
}

// --- actions ------------------------------------------------------------------------

pub(super) async fn dismiss(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	match service::dismiss(&s.db, ctx.user.id, ctx.team.id, true).await {
		Ok(()) => sse::redirect(&overview_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

pub(super) async fn reopen(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	match service::dismiss(&s.db, ctx.user.id, ctx.team.id, false).await {
		Ok(()) => sse::redirect(&overview_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn set_skip(s: AppState, ctx: TeamCtx, item: String, skipped: bool) -> Response {
	let Some(item) = Item::from_slug(&item) else {
		return error::action(&ServerError::NotFound);
	};
	match service::skip(&s.db, ctx.user.id, ctx.team.id, item, skipped).await {
		Ok(()) => {
			let back = match item {
				Item::Networks => Step::Networks,
				_ => Step::Access,
			};
			sse::redirect(&step_url(ctx.team.id, back)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

pub(super) async fn skip(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, item)): Path<(TeamId, String)>,
) -> Response {
	set_skip(s, ctx, item, true).await
}

pub(super) async fn unskip(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, item)): Path<(TeamId, String)>,
) -> Response {
	set_skip(s, ctx, item, false).await
}
