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
