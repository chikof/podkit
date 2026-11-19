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
			message: humanize(&err.to_string()),
		},
	}
}

/// The service layer names fields as the API does (`repo_url`, `git_token`); a person reads
/// sentences. Known messages get their own wording and any other `invalid <field>` becomes
/// "That <field> isn't valid.", so a raw field name never reaches the page. Anything else
/// (the messages that are already sentences) passes through unchanged.
fn humanize(raw: &str) -> String {
	const KNOWN: [(&str, &str); 10] = [
		(
			"invalid repo_url",
			"That repository URL isn't valid. Use an https:// URL, an ssh:// URL or a git@host:owner/repo.git address.",
		),
		(
			"invalid dockerfile_path",
			"That Dockerfile path isn't valid. Use a path inside the repository, like Dockerfile.",
		),
		(
			"invalid image reference",
			"That image reference isn't valid. Use name:tag, like ghcr.io/acme/web:1.2.",
		),
		(
			"set either repo_url or image, not both",
			"Choose a repository or an image, not both.",
		),
		(
			"repo_url or image is required",
			"Give a repository URL or a Docker image.",
		),
		(
			"registry credentials only apply to image applications",
			"Registry credentials only apply to applications that run an image.",
		),
		(
			"deploy_key and git_token only apply to git applications",
			"Deploy keys and access tokens only apply to applications built from a repository.",
		),
		(
			"registry_username and registry_password go together",
			"Give both a registry username and password, or neither.",
		),
		(
			"deploy_key requires an ssh:// or scp-form repo_url, use git_token for https://",
			"A deploy key needs an ssh:// or git@host:owner/repo.git address. For an https:// URL use an access token.",
		),
		(
			"git_token requires an https:// repo_url, use deploy_key for ssh",
			"An access token needs an https:// repository URL. For an ssh address use a deploy key.",
		),
	];
	if let Some((_, text)) = KNOWN.iter().find(|(key, _)| *key == raw) {
		return (*text).to_owned();
	}
	match raw.strip_prefix("invalid ") {
		Some(field)
			if !field.is_empty() && field.chars().all(|c| c.is_ascii_lowercase() || c == '_') =>
		{
			format!("That {} isn't valid.", field.replace('_', " "))
		}
		_ => raw.to_owned(),
	}
}

/// The text a Datastar action shows in its `_error` line. Validation and
/// conflict messages are for the user; internal errors say nothing useful.
pub fn message(err: &ServerError) -> String {
	let v = view(err);
	match v.status {
		StatusCode::NOT_FOUND => "That no longer exists.".to_owned(),
		s if s.is_server_error() && s != StatusCode::BAD_GATEWAY => GENERIC.to_owned(),
		_ => humanize(&v.message),
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

	#[test]
	fn a_raw_field_name_never_reaches_the_person() {
		let invalid = |m: &str| message(&ServerError::Validation(m.to_owned()));
		let repo = invalid("invalid repo_url");
		assert!(
			repo.starts_with("That repository URL isn't valid."),
			"{repo}"
		);
		assert!(!repo.contains("repo_url"), "{repo}");
		// Any other `invalid <field>` is made into a sentence too.
		assert_eq!(invalid("invalid ssh_user"), "That ssh user isn't valid.");
		let key = invalid(
			"deploy_key requires an ssh:// or scp-form repo_url, use git_token for https://",
		);
		assert!(
			!key.contains("deploy_key") && !key.contains("repo_url"),
			"{key}"
		);
		// A message that is already a sentence, or has more to say, is left alone.
		assert_eq!(invalid("Name must be set."), "Name must be set.");
		assert_eq!(
			invalid("invalid model_path \"x\""),
			"invalid model_path \"x\""
		);
	}
}
