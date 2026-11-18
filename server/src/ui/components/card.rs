//! Card: a bordered surface that groups one thing. A card sizes its own layout from the
//! width it is given (container queries in `components.css`), so the same card works in a
//! grid, a sidebar or a dialog.
//!
//! `card(&content)` wraps; build the inside from `card_header`, `card_content` and
//! `card_footer`. `link_card` is the logo card: one resource, the whole card is the link.

use maud::{Markup, html};

pub fn card(content: &Markup) -> Markup {
	html! {
		div class="ui-card flex flex-col gap-5 rounded-xl border border-border bg-card py-6 text-card-foreground shadow-xs" {
			(content)
		}
	}
}

/// Title and description at the top of a card. The title is the page's `<h1>`; inside a
/// page that already has one, use `card_section`.
pub fn card_header(title: &str, description: Option<&str>) -> Markup {
	html! {
		div class="ui-card-header px-6" {
			h1 class="text-base leading-none font-semibold" { (title) }
			@if let Some(description) = description {
				p class="text-sm text-muted-foreground" { (description) }
			}
		}
	}
}

/// Like `card_header` for a card inside a page that already has its `<h1>`: the title is
/// an `<h2>`, so the outline has no second `<h1>` and no skipped level.
pub fn card_heading(title: &str, description: Option<&str>) -> Markup {
	card_section(title, description, None)
}

/// Like `card_header` with an `<h2>` and an optional action (a button or menu) at the
/// end, which wraps below the text when the card is narrow.
pub fn card_section(title: &str, description: Option<&str>, action: Option<&Markup>) -> Markup {
	html! {
		div class="ui-card-header px-6" {
			h2 class="text-base leading-none font-semibold" { (title) }
			@if let Some(description) = description {
				p class="text-sm text-muted-foreground" { (description) }
			}
			@if let Some(action) = action {
				div class="ui-card-action mt-1 flex items-center gap-2" { (action) }
			}
		}
	}
}

/// Just a title, for markup that composes its own header.
pub fn card_title(title: &str) -> Markup {
	html! { h2 class="text-base leading-none font-semibold" { (title) } }
}

pub fn card_description(text: &str) -> Markup {
	html! { p class="text-sm text-muted-foreground" { (text) } }
}

pub fn card_content(content: &Markup) -> Markup {
	html! { div class="px-6" { (content) } }
}

/// Actions at the bottom of a card, aligned to the end.
pub fn card_footer(content: &Markup) -> Markup {
	html! {
		div class="ui-card-footer flex items-center justify-end gap-2 border-t border-border px-6 pt-5" {
			(content)
		}
	}
}

/// The logo card: a service or resource as one clickable tile. `logo` comes from
/// `components::logo`; `meta` is a line of badges or status under the description.
/// The link's `::after` covers the card, so the whole tile is the hit area while screen
/// readers hear one link named by `title`.
pub fn link_card(
	href: &str,
	logo: &Markup,
	title: &str,
	description: Option<&str>,
	meta: Option<&Markup>,
) -> Markup {
	html! {
		article class="ui-card ui-link-card rounded-xl border border-border bg-card p-5 text-card-foreground shadow-xs" {
			div class="ui-link-card-body" {
				(logo)
				div class="flex min-w-0 flex-1 flex-col gap-1" {
					h2 class="truncate text-sm leading-tight font-semibold" {
						a href=(href) class="ui-link-card-link outline-hidden" { (title) }
					}
					@if let Some(description) = description {
						p class="line-clamp-2 text-sm text-muted-foreground" { (description) }
					}
					@if let Some(meta) = meta {
						div class="mt-1 flex flex-wrap items-center gap-2" { (meta) }
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ui::components::{LogoSize, logo};

	#[test]
	fn a_card_wraps_its_content_and_headers_escape_text() {
		let inner =
			html! { (card_header("<b>T</b>", Some("a & b"))) (card_content(&html! { "body" })) };
		let html = card(&inner).into_string();
		assert!(html.contains("ui-card "), "{html}");
		assert!(html.contains("&lt;b&gt;T&lt;/b&gt;"), "{html}");
		assert!(html.contains("a &amp; b"), "{html}");
		assert!(!html.contains("<b>"), "{html}");
	}

	#[test]
	fn a_section_header_is_an_h2_with_an_optional_action() {
		let html = card_section("Env", None, Some(&html! { button { "Add" } })).into_string();
		assert!(html.contains("<h2 "), "{html}");
		assert!(html.contains("ui-card-action"), "{html}");
		assert!(
			!card_section("Env", None, None)
				.into_string()
				.contains("ui-card-action")
		);
	}

	#[test]
	fn a_footer_aligns_actions_to_the_end() {
		let html = card_footer(&html! { button { "Save" } }).into_string();
		assert!(html.contains("justify-end"), "{html}");
	}

	#[test]
	fn a_link_card_is_one_link_named_by_its_title() {
		let html = link_card(
			"/teams/1/projects/2",
			&logo("postgres", LogoSize::Md),
			"Billing <db>",
			Some("Postgres 18"),
			None,
		)
		.into_string();
		assert_eq!(html.matches("<a ").count(), 1, "{html}");
		assert!(html.contains("ui-link-card-link"), "{html}");
		assert!(html.contains("Billing &lt;db&gt;"), "{html}");
		assert!(html.contains("<h2 "), "{html}");
	}

	#[test]
	fn a_card_heading_is_an_h2_so_a_page_keeps_one_h1() {
		let html = card_heading("Members", Some("Everyone")).into_string();
		assert!(html.contains("<h2 "), "{html}");
		assert!(!html.contains("<h1"), "{html}");
		assert!(card_header("Sign in", None).into_string().contains("<h1 "));
	}

	#[test]
	fn a_link_card_href_cannot_break_out_of_its_attribute() {
		let html = link_card("/x\" onclick=\"y", &html! {}, "T", None, None).into_string();
		assert!(!html.contains("onclick=\"y"), "{html}");
	}
}
