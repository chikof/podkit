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
//!
//! What can be created is the [`KINDS`] table: a new kind of resource is one more row, and
//! [`entries`] never grows.

use maud::{Markup, html};

use crate::auth::permission::{Action, Resource};
use crate::environments::Environment;
use crate::ids::{ProjectId, TeamId};
use crate::projects::Project;
use crate::ui::components::{LogoSize, Size, Tone, Variant, badge, button_class, icon, logo};
use crate::ui::extract::TeamCtx;

/// How a choice draws its mark.
#[derive(Clone, Copy)]
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

// --- the table ---------------------------------------------------------------------------

/// Where the caller is: what a link is built from.
struct Place {
	team: TeamId,
	project: ProjectId,
	/// `environment=<id>`, carried by every link.
	query: String,
}

impl Place {
	fn base(&self) -> String {
		format!("/teams/{}/projects/{}", self.team, self.project)
	}
}

/// Where a choice leads.
#[derive(Clone, Copy)]
enum Link {
	/// A page under the project; `extra` is more query, with its own `&`.
	Project {
		path: &'static str,
		extra: &'static str,
	},
	/// A team-level page that is told the project.
	TeamPage { path: &'static str },
}

impl Link {
	fn href(self, at: &Place) -> String {
		match self {
			Self::Project { path, extra } => {
				format!("{}{path}?{}{extra}", at.base(), at.query)
			}
			Self::TeamPage { path } => format!(
				"/teams/{}{path}?project={}&{}",
				at.team, at.project, at.query
			),
		}
	}
}

/// One engine of a database choice.
struct EngineRow {
	key: &'static str,
	title: &'static str,
	mark: &'static str,
	what: &'static str,
}

/// One row of the table. It is offered to a caller who may `create` any of `needs`.
struct Kind {
	key: &'static str,
	title: &'static str,
	summary: &'static str,
	detail: &'static str,
	provide: &'static [&'static str],
	mark: Mark<'static>,
	needs: &'static [Resource],
	/// `None` shows the choice as "Soon", not a link.
	link: Option<Link>,
	/// When not empty, the choice is these links, one per engine, to the database page.
	engines: &'static [EngineRow],
}

impl Kind {
	fn entry(&self, at: &Place) -> Entry<'static> {
		let base = at.base();
		Entry {
			key: self.key,
			title: self.title,
			summary: self.summary,
			detail: self.detail,
			provide: self.provide,
			mark: self.mark,
			href: self.link.map(|l| l.href(at)),
			engines: self
				.engines
				.iter()
				.map(|e| Engine {
					title: e.title,
					what: e.what,
					mark: e.mark,
					href: format!("{base}/databases/new?{}&engine={}", at.query, e.key),
				})
				.collect(),
		}
	}

	fn allowed(&self, ctx: &TeamCtx) -> bool {
		self.needs.iter().any(|r| ctx.perms.can(Action::Create, *r))
	}
}

const ENGINES: &[EngineRow] = &[
	EngineRow {
		key: "postgres",
		title: "PostgreSQL",
		mark: "postgres",
		what: "Relational, the usual default.",
	},
	EngineRow {
		key: "mysql",
		title: "MySQL",
		mark: "mysql",
		what: "Relational, widely hosted.",
	},
	EngineRow {
		key: "mongodb",
		title: "MongoDB",
		mark: "mongodb",
		what: "Document store.",
	},
	EngineRow {
		key: "redis",
		title: "Redis",
		mark: "redis",
		what: "In-memory cache and queue.",
	},
];

/// Everything an environment can hold, in the order of the list.
const KINDS: &[Kind] = &[
	Kind {
		key: "git",
		title: "Git repository",
		summary: "GitHub, GitLab or any Git URL",
		detail: "Builds a Dockerfile or a Nixpacks plan on every push and deploys it behind a domain.",
		provide: &["Repository URL", "Branch", "Build method and port"],
		mark: Mark::Icon("git-branch"),
		needs: &[Resource::Application],
		link: Some(Link::Project {
			path: "/apps/new",
			extra: "",
		}),
		engines: &[],
	},
	Kind {
		key: "image",
		title: "Docker image",
		summary: "From a registry, with auth",
		detail: "Runs an existing image. It is pulled on deploy; private registries work.",
		provide: &["Image and tag", "Port", "Registry credentials, if private"],
		mark: Mark::Logo("docker"),
		needs: &[Resource::Application],
		link: Some(Link::Project {
			path: "/apps/new",
			extra: "&source=image",
		}),
		engines: &[],
	},
	Kind {
		key: "db",
		title: "Database",
		summary: "Postgres, MySQL, MongoDB, Redis",
		detail: "A managed container with a volume, scheduled backups and a connection string. Pick the engine to continue.",
		provide: &[],
		mark: Mark::Icon("database"),
		needs: &[Resource::Database],
		link: Some(Link::Project {
			path: "/databases/new",
			extra: "&engine=postgres",
		}),
		engines: ENGINES,
	},
	Kind {
		key: "stack",
		title: "Compose",
		summary: "Paste or import docker-compose.yml",
		detail: "Runs several services as one unit, with shared environment and a status per service.",
		provide: &["A compose file", "A name for the stack"],
		mark: Mark::Icon("layers"),
		needs: &[Resource::Stack],
		link: Some(Link::Project {
			path: "/stacks/new",
			extra: "",
		}),
		engines: &[],
	},
	Kind {
		key: "template",
		title: "Template",
		summary: "One-click apps from the catalog",
		detail: "Curated apps with generated secrets and a ready domain. You pick one from the catalog next.",
		provide: &[
			"A template from the catalog",
			"A domain, if you want your own",
		],
		mark: Mark::Icon("package"),
		needs: &[Resource::Stack, Resource::Application],
		link: Some(Link::Project {
			path: "/templates",
			extra: "",
		}),
		engines: &[],
	},
	Kind {
		key: "cron",
		title: "Scheduled task",
		summary: "A one-shot container on a cron schedule",
		detail: "Runs a command in a new container on a schedule and removes it afterwards. It can inherit an application's environment and networks. To run a command inside an app instead, use that app's Cron tab.",
		provide: &[
			"An image or an application",
			"A command",
			"A cron schedule and time zone",
		],
		mark: Mark::Icon("clock"),
		needs: &[Resource::Cron],
		link: Some(Link::Project {
			path: "/cron/new",
			extra: "&kind=container",
		}),
		engines: &[],
	},
	Kind {
		key: "model",
		title: "Model",
		summary: "llama.cpp, Ollama, vLLM",
		detail: "Pick a model from the catalog, download its weights and serve it here behind an OpenAI-compatible endpoint.",
		provide: &["A model from the catalog", "An engine and context size"],
		mark: Mark::Logo("huggingface"),
		needs: &[Resource::Model],
		link: Some(Link::TeamPage {
			path: "/models/catalog",
		}),
		engines: &[],
	},
];

/// What the caller may create here, in the order of the list.
fn entries(
	ctx: &TeamCtx,
	team: TeamId,
	project: ProjectId,
	env: &Environment,
) -> Vec<Entry<'static>> {
	let at = Place {
		team,
		project,
		query: format!("environment={}", env.id),
	};
	KINDS
		.iter()
		.filter(|k| k.allowed(ctx))
		.map(|k| k.entry(&at))
		.collect()
}

// --- the page ----------------------------------------------------------------------------

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

/// Whether the caller may create anything here at all; the button is hidden otherwise.
pub fn can_create_any(ctx: &TeamCtx) -> bool {
	KINDS.iter().any(|k| k.allowed(ctx))
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

	fn at() -> Place {
		Place {
			team: TeamId(1),
			project: ProjectId(2),
			query: "environment=3".to_owned(),
		}
	}

	#[test]
	fn followups_picker_table_keys_are_unique_and_every_link_carries_the_environment() {
		let mut keys: Vec<&str> = KINDS.iter().map(|k| k.key).collect();
		keys.sort_unstable();
		keys.dedup();
		assert_eq!(keys.len(), KINDS.len(), "a key is used twice");
		for kind in KINDS {
			let entry = kind.entry(&at());
			assert!(!kind.needs.is_empty(), "{} is offered to nobody", kind.key);
			if let Some(href) = entry.href {
				assert!(href.contains("environment=3"), "{} -> {href}", kind.key);
			}
			for e in entry.engines {
				assert!(e.href.contains("environment=3"), "{}", e.href);
				assert!(e.href.contains("engine="), "{}", e.href);
			}
		}
	}

	#[test]
	fn followups_picker_table_builds_the_links_the_flows_read() {
		let at = at();
		let by = |key: &str| KINDS.iter().find(|k| k.key == key).unwrap().entry(&at);
		assert_eq!(
			by("git").href.as_deref(),
			Some("/teams/1/projects/2/apps/new?environment=3")
		);
		assert_eq!(
			by("image").href.as_deref(),
			Some("/teams/1/projects/2/apps/new?environment=3&source=image")
		);
		assert_eq!(
			by("db").href.as_deref(),
			Some("/teams/1/projects/2/databases/new?environment=3&engine=postgres")
		);
		assert_eq!(by("db").engines.len(), 4);
		assert_eq!(
			by("cron").href.as_deref(),
			Some("/teams/1/projects/2/cron/new?environment=3&kind=container")
		);
		assert_eq!(
			by("model").href.as_deref(),
			Some("/teams/1/models/catalog?project=2&environment=3")
		);
		assert_eq!(
			by("template").href.as_deref(),
			Some("/teams/1/projects/2/templates?environment=3")
		);
	}
}
