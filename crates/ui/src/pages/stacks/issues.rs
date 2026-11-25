//! The validation panel: what podkit refuses in a compose file, key by key, with the
//! service and the path, so the file can be fixed in one pass.

use maud::{Markup, html};

use podkit_core::error::ServerError;

/// One refused key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IssueRow {
	pub service: Option<String>,
	pub path: String,
	pub message: String,
}

/// The refused keys of an `Unprocessable` error, with its headline; `None` for any other
/// error.
pub(super) fn issues_of(e: &ServerError) -> Option<(String, Vec<IssueRow>)> {
	let ServerError::Unprocessable { message, issues } = e else {
		return None;
	};
	let text = |v: &serde_json::Value, key: &str| {
		v.get(key)
			.and_then(serde_json::Value::as_str)
			.map(str::to_owned)
	};
	let rows = issues
		.as_array()
		.map(|list| {
			list.iter()
				.map(|i| IssueRow {
					service: text(i, "service"),
					path: text(i, "path").unwrap_or_default(),
					message: text(i, "message").unwrap_or_default(),
				})
				.collect()
		})
		.unwrap_or_default();
	Some((message.clone(), rows))
}

/// The panel for refused keys. `id` is the element the page patches.
pub(super) fn problems(id: &str, headline: &str, rows: &[IssueRow]) -> Markup {
	html! {
		section id=(id) class="flex flex-col gap-3 rounded-[length:var(--r-md)] border border-destructive/50 p-4" role="alert"
			aria-label="Compose problems" data-state="invalid" {
			div class="flex flex-col gap-1" {
				h3 class="text-sm font-semibold text-destructive" { "Cannot deploy this file" }
				p class="text-sm text-muted-foreground" { (headline) ". Fix these " (rows.len()) @if rows.len() == 1 { " problem" } @else { " problems" } " and check again." }
			}
			@if !rows.is_empty() {
				ul class="flex flex-col gap-2" {
					@for r in rows {
						li class="flex flex-col gap-0.5 text-sm" {
							div class="flex flex-wrap items-center gap-2" {
								@if let Some(service) = &r.service {
									span class="rounded-[length:var(--r-xs)] bg-muted px-1.5 py-0.5 font-mono text-xs" data-issue-service { (service) }
								} @else {
									span class="rounded-[length:var(--r-xs)] bg-muted px-1.5 py-0.5 text-xs" { "whole file" }
								}
								code class="font-mono text-xs" data-issue-path { (r.path) }
							}
							span class="text-muted-foreground" { (r.message) }
						}
					}
				}
			}
		}
	}
}

/// The panel for a file that passed: what podkit will adjust and what it still needs.
pub(super) fn valid(id: &str, notes: &[String], missing: &[String]) -> Markup {
	html! {
		section id=(id) class="flex flex-col gap-3 rounded-[length:var(--r-md)] border border-border p-4" aria-label="Compose check"
			role="status" data-state="valid" {
			h3 class="flex items-center gap-2 text-sm font-semibold text-success" {
				"This file can be deployed"
			}
			@if !missing.is_empty() {
				p class="text-sm text-muted-foreground" {
					"It uses variables that are not set: "
					@for (i, name) in missing.iter().enumerate() {
						@if i > 0 { ", " }
						code class="font-mono text-xs" { (name) }
					}
					". Set them in the variables box or after creating the stack."
				}
			}
			@if !notes.is_empty() {
				ul class="flex list-disc flex-col gap-1 pl-5 text-sm text-muted-foreground" {
					@for n in notes { li { (n) } }
				}
			}
		}
	}
}

/// An empty placeholder the panel is patched into.
pub(super) fn placeholder(id: &str) -> Markup {
	html! { div id=(id) {} }
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn each_issue_names_its_service_path_and_reason_and_is_escaped() {
		let e = ServerError::Unprocessable {
			message: "the compose file uses unsupported features".into(),
			issues: json!([
				{"service": "web", "path": "services.web.build", "key": "build", "message": "build is not supported <b>"},
				{"service": null, "path": "configs", "key": "configs", "message": "top-level configs"},
			]),
		};
		let (headline, rows) = issues_of(&e).expect("unprocessable");
		assert_eq!(rows.len(), 2);
		let html = problems("p", &headline, &rows).into_string();
		assert!(html.contains("services.web.build"), "{html}");
		assert!(html.contains(">web<"), "{html}");
		assert!(html.contains("whole file"), "{html}");
		assert!(
			html.contains("&lt;b&gt;") && !html.contains("<b>"),
			"{html}"
		);
		assert!(html.contains("2 problems"), "{html}");
		assert!(issues_of(&ServerError::NotFound).is_none());
	}
}
