//! Reusable UI pieces, adapted from Topcoat UI's shadcn-style components
//! (MIT, <https://github.com/tokio-rs/topcoat>). They use the semantic colors in
//! `theme.css`.
//!
//! Class lists must stay whole string literals: `build.rs` finds them by
//! scanning this source, so a class assembled at runtime gets no CSS. Elements
//! that carry Datastar attributes (buttons, inputs) take these as `class=`
//! constants, because maud can't forward arbitrary attributes through a
//! function.

use maud::{Markup, html};

macro_rules! button_base {
	() => {
		"inline-flex h-9 shrink-0 cursor-pointer items-center justify-center gap-2 rounded-lg border px-4 text-sm font-medium whitespace-nowrap transition-colors outline-hidden select-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 "
	};
}

/// The main action on a page.
pub const BUTTON: &str = concat!(
	button_base!(),
	"border-transparent bg-primary text-primary-foreground hover:bg-primary/90 active:bg-primary/80"
);

/// A secondary action, drawn as a bordered button.
pub const BUTTON_OUTLINE: &str = concat!(
	button_base!(),
	"border-border text-foreground hover:bg-foreground/5 active:bg-foreground/10"
);

/// A destructive action, drawn as a bordered button in the error color.
pub const BUTTON_DANGER: &str = concat!(
	button_base!(),
	"border-border text-destructive hover:bg-destructive/10 active:bg-destructive/20"
);

pub const INPUT: &str = "h-9 w-full min-w-0 rounded-lg border border-border bg-transparent px-3 text-sm font-normal transition-colors outline-hidden placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50";

/// A `<label>` wrapping its text and control, stacked.
pub const LABEL: &str = "flex flex-col gap-2 text-sm leading-none font-medium";

pub fn card(content: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-5 rounded-xl border border-border bg-card py-6 text-card-foreground" {
			(content)
		}
	}
}

pub fn card_header(title: &str, description: Option<&str>) -> Markup {
	html! {
		div class="flex flex-col gap-1.5 px-6" {
			h1 class="text-base leading-none font-semibold" { (title) }
			@if let Some(description) = description {
				p class="text-sm text-muted-foreground" { (description) }
			}
		}
	}
}

pub fn card_content(content: &Markup) -> Markup {
	html! { div class="px-6" { (content) } }
}

/// How a status reads at a glance. One color per tone, from `theme.css`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
	/// Idle or finished without a verdict: queued, stopped, cancelled.
	Neutral,
	/// Work in progress.
	Progress,
	/// Healthy or completed.
	Success,
	/// Needs attention but is not broken.
	Warning,
	/// Failed.
	Danger,
}

impl Tone {
	pub const fn badge_class(self) -> &'static str {
		match self {
			Self::Neutral => {
				"inline-flex items-center rounded-full border border-border px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-muted-foreground"
			}
			Self::Progress => {
				"inline-flex items-center rounded-full border border-info/30 bg-info/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-info"
			}
			Self::Success => {
				"inline-flex items-center rounded-full border border-success/30 bg-success/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-success"
			}
			Self::Warning => {
				"inline-flex items-center rounded-full border border-warning/30 bg-warning/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-warning"
			}
			Self::Danger => {
				"inline-flex items-center rounded-full border border-destructive/30 bg-destructive/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-destructive"
			}
		}
	}
}
