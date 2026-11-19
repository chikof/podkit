//! Turning a [`ServerError`] into something the browser can show: an HTML
//! page for navigations, an `_error` signal for Datastar actions. Details of
//! internal failures are logged and never sent.

use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::components::{BUTTON, BUTTON_OUTLINE, dialog_open_button, icon, kbd};
use super::extract::{TeamCtx, UiUser};
use super::{layout, sse};
use crate::error::ServerError;

const GENERIC: &str = "Something went wrong. Try again.";

struct View {
	status: StatusCode,
	title: &'static str,
	/// The `<h1>` on the page: a sentence where `title` is a label.
	heading: &'static str,
	message: String,
}

fn view(err: &ServerError) -> View {
	let status = err.status();
	if matches!(err, ServerError::RegistrationClosed) {
		return View {
			status,
			title: "Registration is closed",
			heading: "Registration is closed",
			message: err.to_string(),
		};
	}
	match status {
		StatusCode::NOT_FOUND => View {
			status,
			title: "Not found",
			heading: "This page doesn’t exist",
			message: "It may have been deleted or renamed, or you may not have access to it."
				.to_owned(),
		},
		StatusCode::FORBIDDEN => View {
			status,
			title: "Not allowed",
			heading: "You don’t have access to this",
			message: "You don't have permission to do that.".to_owned(),
		},
		s if s.is_server_error() && s != StatusCode::BAD_GATEWAY => {
			tracing::error!(error = %err, "ui request failed");
			View {
				status,
				title: "Something went wrong",
				heading: "Something went wrong",
				message: "The server hit an unexpected error. Try again in a moment.".to_owned(),
			}
		}
		_ => View {
			status,
			title: "Can't do that",
			heading: "Can’t do that",
			message: err.to_string(),
		},
	}
}

/// The text a Datastar action shows in its `_error` line. Validation and
/// conflict messages are for the user; internal errors say nothing useful.
pub fn message(err: &ServerError) -> String {
	let v = view(err);
	match v.status {
		StatusCode::NOT_FOUND => "That no longer exists.".to_owned(),
		s if s.is_server_error() && s != StatusCode::BAD_GATEWAY => GENERIC.to_owned(),
		_ => v.message,
	}
}

/// The page body: the status, what happened, and where to go from here. `path` is what
/// was asked for (a 404 names it); `back` is where "back" goes; `search` adds the palette
/// button, which only exists inside the team's frame.
fn body(v: &View, path: Option<&str>, back: (&str, &str), search: bool) -> Markup {
	html! {
		section class="ui-nf" aria-labelledby="nf-t" {
			p class="ui-nf-code" { (v.status.as_u16()) }
			h1 id="nf-t" { (v.heading) }
			p class="ui-lede ui-nf-path" {
				@if let (StatusCode::NOT_FOUND, Some(path)) = (v.status, path) {
					"Nothing matches " code { (path) } ". "
				}
				(v.message)
			}
			div class="ui-page-actions" {
				a class={ (BUTTON) } href=(back.0) {
					(icon("arrow-left"))
					span { (back.1) }
				}
				@if search {
					(dialog_open_button("palette", BUTTON_OUTLINE, &html! {
						(icon("search"))
						span { "Search" }
						(kbd("Ctrl K"))
					}))
				}
			}
		}
	}
}

/// An error page for a GET inside the team's layout.
pub fn page(ctx: &TeamCtx, err: &ServerError) -> Response {
	let v = view(err);
	let crumbs = [(v.title, None)];
	let projects = format!("/teams/{}/projects", ctx.team.id);
	let main = body(&v, Some(&ctx.path), (&projects, "Back to Projects"), true);
	(v.status, layout::app_page(ctx, v.title, &crumbs, &main)).into_response()
}

/// An error page with no team around it, for failures before a team is known.
pub fn bare(user: Option<&UiUser>, err: &ServerError) -> Response {
	let v = view(err);
	let main = body(&v, None, ("/", "Back to podkit"), false);
	(
		v.status,
		layout::bare_page(user.map(|u| u.name.as_str()), v.title, &main),
	)
		.into_response()
}

/// A failed Datastar action: the message lands in `_error`.
pub fn action(err: &ServerError) -> Response {
	sse::error(&message(err)).into_response()
}

/// Picks the page or action form for a failure before a handler could.
pub fn respond(method: &Method, user: Option<&UiUser>, err: &ServerError) -> Response {
	if method == Method::GET || method == Method::HEAD {
		bare(user, err)
	} else {
		action(err)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	async fn text(resp: Response) -> (StatusCode, String) {
		let status = resp.status();
		let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
			.await
			.unwrap();
		(status, String::from_utf8(bytes.to_vec()).unwrap())
	}

	#[tokio::test]
	async fn a_500_page_leaks_nothing() {
		let err = ServerError::StdIo(std::io::Error::other("secret path /x"));
		let (status, body) = text(bare(None, &err)).await;
		assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
		assert!(!body.contains("secret path"), "{body}");
		assert!(body.contains("Something went wrong"));
		assert_eq!(message(&err), GENERIC);
	}

	#[tokio::test]
	async fn not_found_and_forbidden_pages_have_their_status() {
		let (s, body) = text(bare(None, &ServerError::ProjectNotFound)).await;
		assert_eq!(s, StatusCode::NOT_FOUND);
		assert!(body.contains("Not found"));
		let (s, body) = text(bare(None, &ServerError::Forbidden)).await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		assert!(body.contains("permission"));
	}

	#[test]
	fn action_messages_pass_user_errors_through() {
		let v = ServerError::Validation("Name must be set.".to_owned());
		assert_eq!(message(&v), "Name must be set.");
		let c = ServerError::Conflict("Slug taken.".to_owned());
		assert_eq!(message(&c), "Slug taken.");
		assert_eq!(
			message(&ServerError::Forbidden),
			"You don't have permission to do that."
		);
		assert_eq!(message(&ServerError::Internal), GENERIC);
	}
}
