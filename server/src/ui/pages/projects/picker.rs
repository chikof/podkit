//! The create-resource picker: a page that lists everything an environment can hold, shows
//! what each choice involves, and sends the pick to the page that creates it. The create
//! flows themselves belong to the application, database, model and template cards; this
//! only routes to them.
//!
//! A Configure surface (the first step of a two-step flow): the choices on the left, the
//! selected one explained on the right, one primary action. The selection is a native radio
//! group; the panel follows the checked radio through `:has()` (`components.css`), so it
//! needs no script.
//!
//! Every link carries `?environment=<id>` so the flow's header shows the right environment.
//! The flows must read it to create the resource there (see the hand-off).

use maud::{Markup, html};

use crate::auth::permission::{Action, Resource};
use crate::environments::Environment;
use crate::ids::{ProjectId, TeamId};
use crate::projects::Project;
use crate::ui::components::{LogoSize, Size, Tone, Variant, badge, button_class, icon, logo};
use crate::ui::extract::TeamCtx;

/// The compose and template pages, as paths under the project (F8). `None` shows the option as
/// "Soon", not a link.
const COMPOSE_HREF: Option<&str> = Some("/stacks/new");
const TEMPLATES_HREF: Option<&str> = Some("/templates");

/// How a choice draws its mark.
enum Mark<'a> {
	Logo(&'a str),
	Icon(&'a str),
}

/// One thing that can be created.
struct Entry<'a> {
	/// The radio's value and the panel's key.
	key: &'a str,
	title: &'a str,
	/// The line under the title in the list.
	summary: &'a str,
	/// What the panel says about it.
	detail: &'a str,
	/// What the next page asks for.
	provide: &'a [&'a str],
	mark: Mark<'a>,
	/// `None`: not available yet.
	href: Option<String>,
	/// A choice that is several links (a database's engines) instead of one.
	engines: Vec<Engine<'a>>,
}

struct Engine<'a> {
	title: &'a str,
	what: &'a str,
	mark: &'a str,
	href: String,
}

fn mark_of(mark: &Mark<'_>, size: LogoSize) -> Markup {
	match mark {
		Mark::Logo(name) => logo(name, size),
		Mark::Icon(name) => icon(name),
	}
}

fn list_item(entry: &Entry<'_>, checked: bool) -> Markup {
	let id = format!("rt-{}", entry.key);
	html! {
		label class="ui-pk-item" for=(id) {
			input type="radio" name="rtype" value=(entry.key) id=(id) checked[checked];
			span class="ui-pk-ic" aria-hidden="true" { (mark_of(&entry.mark, LogoSize::Sm)) }
			span class="ui-pk-t" {
				b { (entry.title) }
				small { (entry.summary) }
			}
			span class="ui-pk-go" aria-hidden="true" { (icon("chevron-right")) }
		}
	}
}

fn panel(entry: &Entry<'_>, cancel: &str) -> Markup {
	html! {
		section class="ui-pk-detail" data-panel=(entry.key) aria-label=(entry.title) {
			span class="ui-pk-big" aria-hidden="true" { (mark_of(&entry.mark, LogoSize::Lg)) }
			h2 { (entry.title) }
			p { (entry.detail) }
			@if entry.engines.is_empty() {
				ul class="ui-pk-provide" aria-label="You will provide" {
					@for item in entry.provide { li { (item) } }
				}
			} @else {
				ul class="ui-pk-engines" aria-label="Engines" {
					@for e in &entry.engines {
						li {
							a href=(e.href) class="ui-pk-engine" {
								span class="ui-pk-ic" aria-hidden="true" { (logo(e.mark, LogoSize::Sm)) }
								span class="ui-pk-t" { b { (e.title) } small { (e.what) } }
								span class="ui-pk-go" aria-hidden="true" { (icon("chevron-right")) }
							}
						}
					}
				}
			}
			div class="ui-page-actions" {
				@if entry.engines.is_empty() {
					@if let Some(href) = &entry.href {
						a href=(href) class=(button_class(Variant::Default, Size::Md)) {
							"Continue" (icon("arrow-right"))
						}
					} @else {
						span class="ui-pk-soon" { (badge(Tone::Neutral, "Soon")) }
					}
				}
				a href=(cancel) class=(button_class(Variant::Ghost, Size::Md)) { "Cancel" }
			}
		}
	}
}

/// What the caller may create here, in the order of the list.
fn entries<'a>(
	ctx: &TeamCtx,
	team: TeamId,
	project: ProjectId,
	env: &Environment,
) -> Vec<Entry<'a>> {
	let base = format!("/teams/{team}/projects/{project}");
	let q = format!("environment={}", env.id);
	let mut out = Vec::new();

	if ctx.perms.can(Action::Create, Resource::Application) {
		out.push(Entry {
			key: "git",
			title: "Git repository",
			summary: "GitHub, GitLab or any Git URL",
			detail: "Builds a Dockerfile or a Nixpacks plan on every push and deploys it behind a domain.",
			provide: &["Repository URL", "Branch", "Build method and port"],
			mark: Mark::Icon("git-branch"),
			href: Some(format!("{base}/apps/new?{q}")),
			engines: Vec::new(),
		});
		out.push(Entry {
			key: "image",
			title: "Docker image",
			summary: "From a registry, with auth",
			detail: "Runs an existing image. It is pulled on deploy; private registries work.",
			provide: &["Image and tag", "Port", "Registry credentials, if private"],
			mark: Mark::Logo("docker"),
			href: Some(format!("{base}/apps/new?{q}&source=image")),
			engines: Vec::new(),
		});
	}
	if ctx.perms.can(Action::Create, Resource::Database) {
		let engines = [
			(
				"postgres",
				"PostgreSQL",
				"postgres",
				"Relational, the usual default.",
			),
			("mysql", "MySQL", "mysql", "Relational, widely hosted."),
			("mongodb", "MongoDB", "mongodb", "Document store."),
			("redis", "Redis", "redis", "In-memory cache and queue."),
		]
		.into_iter()
		.map(|(engine, title, mark, what)| Engine {
			title,
			what,
			mark,
			href: format!("{base}/databases/new?{q}&engine={engine}"),
		})
		.collect();
		out.push(Entry {
			key: "db",
			title: "Database",
			summary: "Postgres, MySQL, MongoDB, Redis",
			detail: "A managed container with a volume, scheduled backups and a connection string. Pick the engine to continue.",
			provide: &[],
			mark: Mark::Icon("database"),
			href: Some(format!("{base}/databases/new?{q}&engine=postgres")),
			engines,
		});
	}
	if ctx.perms.can(Action::Create, Resource::Stack) {
		out.push(Entry {
			key: "stack",
			title: "Compose",
			summary: "Paste or import docker-compose.yml",
			detail: "Runs several services as one unit, with shared environment and a status per service.",
			provide: &["A compose file", "A name for the stack"],
			mark: Mark::Icon("layers"),
			href: COMPOSE_HREF.map(|h| format!("{base}{h}?{q}")),
			engines: Vec::new(),
		});
	}
	if ctx.perms.can(Action::Create, Resource::Stack)
		|| ctx.perms.can(Action::Create, Resource::Application)
	{
		out.push(Entry {
			key: "template",
			title: "Template",
			summary: "One-click apps from the catalog",
			detail: "Curated apps with generated secrets and a ready domain. You pick one from the catalog next.",
			provide: &["A template from the catalog", "A domain, if you want your own"],
			mark: Mark::Icon("package"),
			href: TEMPLATES_HREF.map(|h| format!("{base}{h}?{q}")),
			engines: Vec::new(),
		});
	}
	if ctx.perms.can(Action::Create, Resource::Model) {
		out.push(Entry {
			key: "model",
			title: "Model",
			summary: "llama.cpp, Ollama, vLLM",
			detail: "Pick a model from the catalog, download its weights and serve it here behind an OpenAI-compatible endpoint.",
			provide: &["A model from the catalog", "An engine and context size"],
			mark: Mark::Logo("huggingface"),
			href: Some(format!(
				"/teams/{team}/models/catalog?project={project}&{q}"
			)),
			engines: Vec::new(),
		});
	}
	out
}

/// Whether the caller may create anything here at all; the button is hidden otherwise.
pub fn can_create_any(ctx: &TeamCtx) -> bool {
	[
		Resource::Application,
		Resource::Database,
		Resource::Model,
		Resource::Stack,
	]
	.into_iter()
	.any(|r| ctx.perms.can(Action::Create, r))
}

/// The button that opens the picker.
pub fn open_button(href: &str) -> Markup {
	html! {
		a href=(href) class=(button_class(Variant::Default, Size::Md)) {
			(icon("plus")) "Create resource"
		}
	}
}

/// The body of the picker page.
pub fn page(ctx: &TeamCtx, project: &Project, env: &Environment) -> Markup {
	let entries = entries(ctx, ctx.team.id, project.id, env);
	let cancel = format!(
		"/teams/{}/projects/{}/{}",
		ctx.team.id, project.id, env.slug
	);
	let first = entries.iter().position(|e| e.href.is_some()).unwrap_or(0);
	let templates = entries
		.iter()
		.find(|e| e.key == "template")
		.and_then(|e| e.href.clone());
	html! {
		div class="ui-wide ui-pk-page" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { "Step 1 of 2" }
					h1 { "Create resource" }
					p class="ui-lede" {
						"in " (project.name) " / " (env.name) " · choose what to run"
					}
				}
				@if let Some(href) = templates {
					div class="ui-page-actions" {
						a href=(href) class=(button_class(Variant::Secondary, Size::Md)) {
							(icon("package")) "Browse templates"
						}
					}
				}
			}
			div class="ui-pk" {
				fieldset class="ui-pk-list" {
					legend class="ui-visually-hidden" { "Resource type" }
					@for (i, entry) in entries.iter().enumerate() {
						(list_item(entry, i == first))
					}
				}
				div class="ui-pk-panels" {
					@for entry in &entries { (panel(entry, &cancel)) }
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_mark_that_is_an_icon_and_one_that_is_a_logo_both_render() {
		let html = mark_of(&Mark::Icon("package"), LogoSize::Sm).into_string();
		assert!(html.contains("data-icon=\"package\""), "{html}");
		let html = mark_of(&Mark::Logo("docker"), LogoSize::Sm).into_string();
		assert!(html.contains("data-logo="), "{html}");
	}
}
