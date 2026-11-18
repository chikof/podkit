//! Breadcrumb: where this page sits, from the section down. Every crumb but the last is a
//! link; the last is the current page (`aria-current="page"`). Separators are decoration.

use maud::{Markup, html};

use super::icon::icon;

/// `(label, Some(href))` for a link, `(label, None)` for the current page.
pub fn breadcrumb(crumbs: &[(&str, Option<String>)]) -> Markup {
	html! {
		nav aria-label="Breadcrumb" {
			ol class="flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground" {
				@for (i, (label, href)) in crumbs.iter().enumerate() {
					li class="inline-flex items-center gap-1.5" {
						@if i > 0 { span class="text-muted-foreground/60" { (icon("chevron-right")) } }
						@if let Some(href) = href {
							a href=(href) class="rounded-sm outline-hidden transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50" { (label) }
						} @else {
							span class="font-medium text-foreground" aria-current="page" { (label) }
						}
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn links_come_first_and_the_last_crumb_is_the_current_page() {
		let html = breadcrumb(&[
			("Projects", Some("/teams/1/projects".to_owned())),
			("api", Some("/teams/1/projects/2".to_owned())),
			("Settings", None),
		])
		.into_string();
		assert!(html.contains("<nav aria-label=\"Breadcrumb\">"), "{html}");
		assert_eq!(html.matches("<a ").count(), 2, "{html}");
		assert_eq!(html.matches("aria-current=\"page\"").count(), 1, "{html}");
		assert_eq!(html.matches("aria-hidden=\"true\"").count(), 2, "{html}");
	}

	#[test]
	fn labels_and_hrefs_are_escaped() {
		let html = breadcrumb(&[("<b>x</b>", Some("/\"y".to_owned())), ("z", None)]).into_string();
		assert!(!html.contains("<b>"), "{html}");
		assert!(html.contains("href=\"/&quot;y\""), "{html}");
	}
}
