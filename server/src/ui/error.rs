//! Turning a [`ServerError`] into something the browser can show: an HTML
//! page for navigations, an `_error` signal for Datastar actions. Details of
//! internal failures are logged and never sent.

use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::extract::{TeamCtx, UiUser};
use super::{layout, sse};
use crate::error::ServerError;

const GENERIC: &str = "Something went wrong. Try again.";

struct View {
	status: StatusCode,
	title: &'static str,
	message: String,
}

fn view(err: &ServerError) -> View {
	let status = err.status();
	match status {
		StatusCode::NOT_FOUND => View {
			status,
			title: "Not found",
			message: "This page doesn't exist, or you don't have access to it.".to_owned(),
		},
		StatusCode::FORBIDDEN => View {
			status,
			title: "Not allowed",
			message: "You don't have permission to do that.".to_owned(),
		},
		s if s.is_server_error() && s != StatusCode::BAD_GATEWAY => {
			tracing::error!(error = %err, "ui request failed");
			View {
				status,
				title: "Something went wrong",
				message: "The server hit an unexpected error. Try again in a moment.".to_owned(),
			}
		}
		_ => View {
			status,
			title: "Can't do that",
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

fn body(v: &View) -> Markup {
	html! {
		div class="flex flex-col gap-3 py-12" {
			h1 class="text-2xl font-semibold" { (v.title) }
			p class="text-muted-foreground" { (v.message) }
			a class="text-sm font-medium hover:underline" href="/" { "Back to podkit" }
		}
	}
}

/// An error page for a GET inside the team's layout.
pub fn page(ctx: &TeamCtx, err: &ServerError) -> Response {
	let v = view(err);
	let crumbs = [(v.title, None)];
	(v.status, layout::app_page(ctx, v.title, &crumbs, &body(&v))).into_response()
}

/// An error page with no team around it, for failures before a team is known.
pub fn bare(user: Option<&UiUser>, err: &ServerError) -> Response {
	let v = view(err);
	(
		v.status,
		layout::bare_page(user.map(|u| u.name.as_str()), v.title, &body(&v)),
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
