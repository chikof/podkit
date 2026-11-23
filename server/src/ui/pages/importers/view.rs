//! How an import's report looks: what was read, then what did not map, grouped by how much it
//! matters. The plan and the apply report under it are [`crate::ui::pages::iac`]'s views.
//!
//! Everything in a note (a name, a message) came from another platform's API, so all of it goes
//! through maud's escaping. Nothing here is `PreEscaped`, and a secret is not in a report.

use maud::{Markup, html};

use crate::importers::{Counts, Level, Note, Report, Source};
use crate::ui::components::Tone;

pub const RESULT: &str = "import-result";

fn level_name(level: Level) -> &'static str {
	match level {
		Level::Skipped => "Left out",
		Level::Unmapped => "Not supported",
		Level::Check => "Check",
	}
}

fn level_help(level: Level) -> &'static str {
	match level {
		Level::Skipped => "These were not imported at all.",
		Level::Unmapped => "podkit has no field for these settings, so they were dropped.",
		Level::Check => "Imported, but look at these before the first deploy.",
	}
}

fn level_tone(level: Level) -> Tone {
	match level {
		Level::Skipped => Tone::Danger,
		Level::Unmapped => Tone::Warning,
		Level::Check => Tone::Progress,
	}
}

fn count(n: usize, one: &str, many: &str) -> String {
	format!("{n} {}", if n == 1 { one } else { many })
}

fn counts(c: &Counts) -> Markup {
	let parts = [
		(c.projects, "project", "projects"),
		(c.environments, "environment", "environments"),
		(c.apps, "application", "applications"),
		(c.databases, "database", "databases"),
		(c.stacks, "stack", "stacks"),
		(c.domains, "domain", "domains"),
		(c.volumes, "volume", "volumes"),
		(c.variables, "variable", "variables"),
	];
	html! {
		ul class="ui-import-counts" aria-label="Read from the source" {
			@for (n, one, many) in parts {
				@if n > 0 { li { (count(n, one, many)) } }
			}
		}
	}
}

fn group(level: Level, notes: &[&Note]) -> Markup {
	html! {
		section class="ui-import-group" aria-label=(level_name(level)) {
			h3 class="ui-iac-sub" {
				span class=(level_tone(level).badge_class()) { (level_name(level)) }
				" " span class="ui-hint" { (notes.len()) }
			}
			p class="ui-hint" { (level_help(level)) }
			ul class="ui-import-notes" {
				@for n in notes {
					li {
						@if !n.at.is_empty() { code class="font-mono text-xs" { (n.at) } " " }
						(n.message)
					}
				}
			}
		}
	}
}

/// What was read, and what did not map.
pub fn report(source: Source, r: &Report) -> Markup {
	let of = |level: Level| -> Vec<&Note> { r.notes.iter().filter(|n| n.level == level).collect() };
	html! {
		div class="ui-iac" {
			p class="ui-iac-sum" role="status" {
				span class=(Tone::Success.badge_class()) { "Read from " (source.label()) }
				span class="ui-hint" { "Nothing has been made yet." }
			}
			(counts(&r.counts))
			@for level in [Level::Skipped, Level::Unmapped, Level::Check] {
				@let notes = of(level);
				@if !notes.is_empty() { (group(level, &notes)) }
			}
		}
	}
}
