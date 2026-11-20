//! How a plan and an apply report look: a summary, the issues that stop an apply, then one
//! block per object with the fields that differ as a diff.
//!
//! Everything that came from a file or a project (names, field values, messages) goes through
//! maud's escaping; nothing here is `PreEscaped`. A secret is never in a plan, so it is never
//! on this page.

use maud::{Markup, html};
use serde_json::Value;

use crate::iac::apply::{Done, Report};
use crate::iac::plan::{Action, Change, Extra, Plan};
use crate::iac::service::{Applied, ProjectPlan};
use crate::ui::components::{BannerKind, Tone, banner, icon};

pub const RESULT: &str = "iac-result";

fn tone(action: Action) -> Tone {
	match action {
		Action::Create => Tone::Success,
		Action::Update => Tone::Warning,
		Action::Delete => Tone::Danger,
	}
}

fn label(action: Action) -> &'static str {
	match action {
		Action::Create => "Create",
		Action::Update => "Update",
		Action::Delete => "Delete",
	}
}

fn kind_name(kind: crate::iac::objects::Kind) -> &'static str {
	match kind {
		crate::iac::objects::Kind::App => "Application",
		crate::iac::objects::Kind::Database => "Database",
		crate::iac::objects::Kind::Stack => "Stack",
		crate::iac::objects::Kind::Model => "Model",
		crate::iac::objects::Kind::Network => "Network",
		crate::iac::objects::Kind::Environment => "Environment",
	}
}

/// A value as a line of text: strings as they are, everything else as JSON.
fn shown(value: &Value) -> String {
	match value {
		Value::String(s) => s.clone(),
		other => other.to_string(),
	}
}

fn value(v: &Value) -> Markup {
	let text = shown(v);
	html! {
		@if text.contains('\n') {
			pre class="ui-iac-pre" { (text) }
		} @else {
			span class="ui-iac-val" { (text) }
		}
	}
}

fn line(sign: &str, field: &str, v: &Value) -> Markup {
	html! {
		li class="ui-iac-line" data-sign=(sign) {
			span class="ui-iac-sign" aria-hidden="true" { (sign) }
			span class="sr-only" { (if sign == "+" { "Added" } else { "Removed" }) }
			span class="ui-iac-field" { (field) }
			(value(v))
		}
	}
}

fn fields(change: &Change) -> Markup {
	html! {
		ul class="ui-iac-fields" aria-label="Fields" {
			@for f in &change.fields {
				@match (&f.from, &f.to) {
					(Some(from), Some(to)) => {
						(line("-", &f.field, from))
						(line("+", &f.field, to))
					}
					(None, Some(to)) => (line("+", &f.field, to)),
					(Some(from), None) => (line("-", &f.field, from)),
					(None, None) => {}
				}
			}
		}
	}
}

fn change(c: &Change) -> Markup {
	html! {
		li class="ui-iac-change" data-action=(label(c.action).to_lowercase()) {
			div class="ui-iac-head" {
				span class=(tone(c.action).badge_class()) { (label(c.action)) }
				span class="ui-iac-kind" { (kind_name(c.kind)) }
				strong class="ui-iac-name" { (c.name) }
				@if let Some(env) = &c.environment {
					@if c.kind != crate::iac::objects::Kind::Environment {
						span class="ui-iac-where" { (env) }
					}
				}
				@if c.will_redeploy && c.action != Action::Delete {
					span class=(Tone::Progress.badge_class()) { "Will redeploy" }
				}
			}
			@if !c.fields.is_empty() { (fields(c)) }
		}
	}
}

fn extra(e: &Extra) -> Markup {
	html! {
		li class="ui-iac-extra-row" {
			span class=(Tone::Neutral.badge_class()) { (kind_name(e.kind)) }
			strong class="ui-iac-name" { (e.name) }
			@if let Some(env) = &e.environment { span class="ui-iac-where" { (env) } }
			@if e.fields.is_empty() {
				span class="ui-iac-where" { "is not in the file" }
			} @else {
				span class="ui-iac-where" {
					"keeps " (e.fields.join(", ")) ", which the file leaves out"
				}
			}
		}
	}
}

fn count(n: usize, one: &str, many: &str) -> String {
	format!("{n} {}", if n == 1 { one } else { many })
}

/// The plan: what an apply would do.
pub fn plan(p: &Plan) -> Markup {
	html! {
		div class="ui-iac" {
			@if p.has_issues() {
				div class="ui-banner" role="alert" {
					(icon("circle-alert"))
					div class="ui-banner-body" {
						b { (count(p.issues.len(), "thing to fix", "things to fix")) } " before this can be applied. Nothing has changed."
						ul class="ui-iac-issues" {
							@for i in &p.issues {
								li { code class="font-mono text-xs" { (i.path) } " " (i.message) }
							}
						}
					}
				}
			}
			p class="ui-iac-sum" role="status" {
				@if p.is_empty() {
					span class=(Tone::Success.badge_class()) { "Up to date" }
					span { "The project already matches the file." }
				} @else {
					span class=(Tone::Success.badge_class()) { (p.summary.create) " to create" }
					span class=(Tone::Warning.badge_class()) { (p.summary.update) " to update" }
					span class=(Tone::Danger.badge_class()) { (p.summary.delete) " to delete" }
				}
				@if p.summary.unchanged > 0 {
					span class="ui-hint" { (p.summary.unchanged) " unchanged" }
				}
			}
			@if !p.changes.is_empty() {
				ul class="ui-iac-changes" aria-label="Changes" {
					@for c in &p.changes { (change(c)) }
				}
			}
			@if !p.extra.is_empty() {
				section class="ui-iac-leftover" aria-label="Left alone" {
					h3 class="ui-iac-sub" { "Left alone" }
					p class="ui-hint" {
						"These exist and the file does not say so. Prune is off, so applying keeps them."
					}
					ul class="ui-iac-extra" { @for e in &p.extra { (extra(e)) } }
				}
			}
		}
	}
}

fn done(d: &Done) -> Markup {
	let action = match d.action {
		Action::Create => Tone::Success,
		Action::Update => Tone::Warning,
		Action::Delete => Tone::Danger,
	};
	html! {
		li class="ui-iac-extra-row" {
			span class=(action.badge_class()) { (label(d.action)) }
			span class="ui-iac-kind" { (kind_name(d.kind)) }
			strong class="ui-iac-name" { (d.name) }
			@if let Some(env) = &d.environment { span class="ui-iac-where" { (env) } }
		}
	}
}

/// What an apply did.
pub fn report(r: &Report) -> Markup {
	html! {
		div class="ui-iac" {
			@if let Some(f) = &r.failed {
				(banner(
					BannerKind::Error,
					Some(&format!("Stopped at {} {}.", kind_name(f.kind), f.name)),
					&f.message,
					None,
				))
			} @else if r.applied.is_empty() {
				(banner(BannerKind::Success, Some("Nothing to apply."), "The project already matches the file.", None))
			} @else {
				(banner(
					BannerKind::Success,
					Some("Applied."),
					&format!("{} done.", count(r.applied.len(), "change", "changes")),
					None,
				))
			}
			@if !r.applied.is_empty() {
				section aria-label="Done" {
					h3 class="ui-iac-sub" { "Done" }
					ul class="ui-iac-extra" { @for d in &r.applied { (done(d)) } }
				}
			}
			@if !r.deployed.is_empty() {
				section aria-label="Deploys" {
					h3 class="ui-iac-sub" { "Deploys started" }
					ul class="ui-iac-extra" { @for d in &r.deployed { (done(d)) } }
				}
			}
			@if !r.not_run.is_empty() {
				section aria-label="Not run" {
					h3 class="ui-iac-sub" { "Not run" }
					p class="ui-hint" { "The apply stopped before these. Fix the error and apply again: what is done is not done twice." }
					ul class="ui-iac-extra" { @for d in &r.not_run { (done(d)) } }
				}
			}
		}
	}
}

/// The result region: an apply's report under the plan it followed.
pub fn applied(a: &Applied) -> Markup {
	html! {
		div id=(RESULT) aria-live="polite" {
			(report(&a.report))
		}
	}
}

/// The result region of a plan; `apply` is the button that goes with it.
pub fn planned(p: &Plan, apply: Option<&Markup>) -> Markup {
	html! {
		div id=(RESULT) aria-live="polite" {
			(plan(p))
			@if let Some(apply) = apply {
				@if !p.has_issues() && !p.changes.is_empty() {
					div class="ui-iac-foot" { (apply) }
				}
			}
		}
	}
}

/// The result region of an empty page.
pub fn empty() -> Markup {
	html! { div id=(RESULT) aria-live="polite" {} }
}

/// A refused plan or a team import: one plan per project.
pub fn projects(plans: &[ProjectPlan], apply: Option<&Markup>) -> Markup {
	let ok = plans.iter().all(|p| !p.plan.has_issues());
	html! {
		div id=(RESULT) aria-live="polite" {
			div class="ui-iac" {
				@for p in plans {
					section class="ui-iac-project" {
						h3 class="ui-iac-sub" {
							(p.name) " " span class="ui-iac-where" { (p.slug) }
						}
						(plan(&p.plan))
					}
				}
			}
			@if let Some(apply) = apply {
				@if ok && plans.iter().any(|p| !p.plan.changes.is_empty()) {
					div class="ui-iac-foot" { (apply) }
				}
			}
		}
	}
}

/// What an import did, project by project.
pub fn imported(done: &[crate::iac::service::Imported]) -> Markup {
	html! {
		div id=(RESULT) aria-live="polite" {
			div class="ui-iac" {
				@for d in done {
					section class="ui-iac-project" {
						h3 class="ui-iac-sub" { (d.slug) }
						@if d.applied.plan.has_issues() {
							(plan(&d.applied.plan))
						} @else {
							(report(&d.applied.report))
						}
					}
				}
			}
		}
	}
}
