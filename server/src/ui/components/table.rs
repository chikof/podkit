//! Table: semantic `<table>` with a sticky header and a row header, in a scrollable
//! region that a keyboard can focus. Below 36rem of its own width the table stacks: each
//! row becomes a block and every cell shows its column name (`data-label`), so nothing
//! needs sideways scrolling on a phone.

use maud::{Markup, html};

/// `caption` names the table for assistive technology and for the focusable scroll region
/// (it is visually hidden: the page's own heading is the visible title). `headers` are the
/// column names; the first cell of every row is the row header. A row shorter than
/// `headers` simply has fewer cells.
pub fn table(id: &str, caption: &str, headers: &[&str], rows: &[Vec<Markup>]) -> Markup {
	let caption_id = format!("{id}-caption");
	html! {
		div class="ui-table-wrap" tabindex="0" role="region" aria-labelledby=(caption_id) id=(id) {
			table class="ui-table" {
				caption id=(caption_id) class="ui-visually-hidden" { (caption) }
				thead {
					tr {
						@for header in headers {
							th scope="col" { (header) }
						}
					}
				}
				tbody {
					@for row in rows {
						tr {
							@for (i, cell) in row.iter().enumerate() {
								@if i == 0 {
									th scope="row" { (cell) }
								} @else {
									td data-label=[headers.get(i)] { (cell) }
								}
							}
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

	fn sample() -> Markup {
		table(
			"apps",
			"Applications",
			&["Name", "Status", "Updated"],
			&[
				vec![html! { "api" }, html! { "Running" }, html! { "now" }],
				vec![html! { "<b>web</b>" }, html! { "Failed" }],
			],
		)
	}

	#[test]
	fn the_region_is_named_by_the_caption_and_focusable() {
		let html = sample().into_string();
		assert!(html.contains("role=\"region\""), "{html}");
		assert!(html.contains("tabindex=\"0\""), "{html}");
		assert!(html.contains("aria-labelledby=\"apps-caption\""), "{html}");
		assert!(html.contains("<caption id=\"apps-caption\""), "{html}");
	}

	#[test]
	fn headers_are_column_headers_and_the_first_cell_is_the_row_header() {
		let html = sample().into_string();
		assert_eq!(html.matches("<th scope=\"col\">").count(), 3, "{html}");
		assert_eq!(html.matches("<th scope=\"row\">").count(), 2, "{html}");
		assert!(
			html.contains("<td data-label=\"Status\">Running</td>"),
			"{html}"
		);
		assert!(
			html.contains("<td data-label=\"Status\">Failed</td>"),
			"{html}"
		);
	}

	#[test]
	fn cells_given_as_text_are_escaped() {
		let html = table("t", "<script>", &["<h>"], &[vec![html! { "<b>x</b>" }]]).into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("&lt;h&gt;"), "{html}");
	}
}
