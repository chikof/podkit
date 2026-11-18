//! The create-resource picker: one dialog that lists everything an environment can hold and
//! sends each choice to the page that creates it. The create flows themselves belong to the
//! application, database, model and template cards; this only routes to them.
//!
//! Every link carries `?environment=<id>` so the flow's header shows the right
//! environment. The flows must read it to create the resource there (see the hand-off).

use maud::{Markup, html};

use crate::auth::permission::{Action, Resource};
use crate::environments::Environment;
use crate::ids::{ProjectId, TeamId};
use crate::ui::components::{LogoSize, Size, Tone, Variant, badge, button_class, icon, logo};
use crate::ui::extract::TeamCtx;

pub const DIALOG_ID: &str = "create-resource";

/// TODO(B2): flip to `true` once the image-based application API (and its create page)
/// exists. Until then the option stays out of the picker rather than leading nowhere.
const DOCKER_IMAGE_READY: bool = false;

/// The compose and template pages, as paths under the project (F8). `None` shows the option as
/// "Soon", not a link.
const COMPOSE_HREF: Option<&str> = Some("/stacks/new");
const TEMPLATES_HREF: Option<&str> = Some("/templates");

/// How a tile draws its mark.
enum Mark<'a> {
	Logo(&'a str),
	Icon(&'a str),
}

struct Choice<'a> {
	title: &'a str,
	description: &'a str,
	mark: Mark<'a>,
	/// `None`: not available yet.
	href: Option<String>,
}

fn tile(choice: &Choice<'_>, first: bool) -> Markup {
	let mark = match &choice.mark {
		Mark::Logo(name) => logo(name, LogoSize::Md),
		Mark::Icon(name) => html! {
			span class="ui-pick-icon" aria-hidden="true" { (icon(name)) }
		},
	};
	html! {
		@if let Some(href) = &choice.href {
			a href=(href) class="ui-pick" autofocus[first] {
				(mark)
				span class="flex min-w-0 flex-col gap-0.5" {
					span class="text-sm font-medium" { (choice.title) }
					span class="text-xs text-muted-foreground" { (choice.description) }
				}
			}
		} @else {
			div class="ui-pick" aria-disabled="true" {
				(mark)
				span class="flex min-w-0 flex-col gap-0.5" {
					span class="flex items-center gap-2 text-sm font-medium" {
						(choice.title)
						(badge(Tone::Neutral, "Soon"))
					}
					span class="text-xs text-muted-foreground" { (choice.description) }
				}
			}
		}
	}
}

fn group(heading: &str, choices: &[Choice<'_>], first: &mut bool) -> Markup {
	html! {
		@if !choices.is_empty() {
			section class="flex flex-col gap-2" {
				h3 class="text-xs font-medium tracking-wide text-muted-foreground uppercase" { (heading) }
				div class="ui-pick-grid" {
					@for c in choices {
						(tile(c, std::mem::take(first) && c.href.is_some()))
					}
				}
			}
		}
	}
}

/// What the caller may create here, as groups of choices.
fn groups<'a>(
	ctx: &TeamCtx,
	team: TeamId,
	project: ProjectId,
	env: &Environment,
) -> Vec<(&'static str, Vec<Choice<'a>>)> {
	let base = format!("/teams/{team}/projects/{project}");
	let q = format!("environment={}", env.id);
	let mut out = Vec::new();

	let mut apps = Vec::new();
	if ctx.perms.can(Action::Create, Resource::Application) {
		apps.push(Choice {
			title: "Git repository",
			description: "Build and deploy an application from a Git repository.",
			mark: Mark::Icon("git-branch"),
			href: Some(format!("{base}/apps/new?{q}")),
		});
		if DOCKER_IMAGE_READY {
			apps.push(Choice {
				title: "Docker image",
				description: "Run an image from a registry.",
				mark: Mark::Logo("docker"),
				href: Some(format!("{base}/apps/new?{q}&source=image")),
			});
		}
	}
	out.push(("Application", apps));

	let mut dbs = Vec::new();
	if ctx.perms.can(Action::Create, Resource::Database) {
		for (engine, title, mark, what) in [
			(
				"postgres",
				"PostgreSQL",
				"postgres",
				"Relational, the usual default.",
			),
			("mysql", "MySQL", "mysql", "Relational, widely hosted."),
			("mongodb", "MongoDB", "mongodb", "Document store."),
			("redis", "Redis", "redis", "In-memory cache and queue."),
		] {
			dbs.push(Choice {
				title,
				description: what,
				mark: Mark::Logo(mark),
				href: Some(format!("{base}/databases/new?{q}&engine={engine}")),
			});
		}
	}
	out.push(("Database", dbs));

	let mut stacks = Vec::new();
	if ctx.perms.can(Action::Create, Resource::Stack) {
		stacks.push(Choice {
			title: "Compose",
			description: "Deploy a stack from a compose file.",
			mark: Mark::Logo("docker"),
			href: COMPOSE_HREF.map(|h| format!("{base}{h}?{q}")),
		});
	}
	if ctx.perms.can(Action::Create, Resource::Stack)
		|| ctx.perms.can(Action::Create, Resource::Application)
	{
		stacks.push(Choice {
			title: "Template",
			description: "Start from a ready-made service in the catalog.",
			mark: Mark::Icon("package"),
			href: TEMPLATES_HREF.map(|h| format!("{base}{h}?{q}")),
		});
	}
	out.push(("Compose and templates", stacks));

	let mut models = Vec::new();
	if ctx.perms.can(Action::Create, Resource::Model) {
		models.push(Choice {
			title: "Model",
			description: "Pick a model from the catalog, download it, and serve it here.",
			mark: Mark::Logo("huggingface"),
			href: Some(format!(
				"/teams/{team}/models/catalog?project={project}&{q}"
			)),
		});
	}
	out.push(("Model", models));
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

/// The button that opens the picker (uses the invoker command, with the Datastar fallback).
pub fn open_button() -> Markup {
	crate::ui::components::dialog_open_button(
		DIALOG_ID,
		&button_class(Variant::Default, Size::Md),
		&html! { (icon("plus")) "Create" },
	)
}

pub fn dialog(ctx: &TeamCtx, project: ProjectId, env: &Environment) -> Markup {
	let mut first = true;
	let groups = groups(ctx, ctx.team.id, project, env);
	html! {
		dialog id=(DIALOG_ID) class="ui-dialog ui-dialog-wide" closedby="any"
			aria-labelledby="create-resource-title" aria-describedby="create-resource-description" {
			div class="flex flex-col gap-5 p-6" {
				div class="flex flex-col gap-1.5 pr-8" {
					h2 id="create-resource-title" class="text-lg leading-none font-semibold" { "Create a resource" }
					p id="create-resource-description" class="text-sm text-muted-foreground" {
						"It goes in the " strong { (env.name) } " environment."
					}
				}
				@for (heading, choices) in &groups {
					(group(heading, choices, &mut first))
				}
			}
			form method="dialog" class="absolute top-3 right-3" {
				button class="inline-flex h-8 w-8 cursor-pointer items-center justify-center rounded-md text-muted-foreground outline-hidden hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring/50" aria-label="Close" {
					(icon("x"))
				}
			}
		}
	}
}
