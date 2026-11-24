//! The page an invite link opens (`/invite/{token}`): sign up as the invited address, or
//! accept with the account that already has it. Whether the link works, and what it allows,
//! is decided by `members::invites`.
//!
//! The token is in the path, so the page is never cached, sends no referrer and may not be
//! framed. Only a token of the shape podkit makes (plain hex) is ever written back into the
//! page. A person who has to sign in first is not sent through `/login?next=` with the token:
//! the page leaves it in a short-lived `HttpOnly`, `SameSite=Strict` cookie scoped to
//! `/invite`, and sign-in comes back to plain `/invite`, which reads it from there.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use tower_cookies::cookie::SameSite;
use tower_cookies::{Cookie, Cookies};

use crate::auth::{self, SessionToken, session};
use crate::error::ServerError;
use crate::members::invites::{self, Preview};
use crate::state::AppState;
use crate::ui::components::{BUTTON, INPUT, LABEL, icon, reveal_button};
use crate::ui::layout::{self, form_banner};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/invite", get(pending))
		.route("/invite/{token}", get(page).post(sign_up))
		.route("/invite/{token}/accept", post(accept))
}

/// No cache keeps the page, and a link followed from it does not carry the token along.
fn private(mut response: Response) -> Response {
	let headers = response.headers_mut();
	headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	headers.insert(
		header::REFERRER_POLICY,
		HeaderValue::from_static("no-referrer"),
	);
	headers.insert(
		header::CONTENT_SECURITY_POLICY,
		HeaderValue::from_static("frame-ancestors 'none'"),
	);
	response
}

/// The cookie that holds a pending invite while its holder signs in.
const PENDING: &str = "invite";
/// Where sign-in returns to for a pending invite: no token in it.
const RETURN: &str = "/invite";

fn remember(cookies: &Cookies, headers: &HeaderMap, token: &str) {
	cookies.add(
		Cookie::build((PENDING, token.to_owned()))
			.http_only(true)
			.secure(auth::reached_over_https(headers))
			.same_site(SameSite::Strict)
			.max_age(time::Duration::hours(1))
			.path(RETURN)
			.build(),
	);
}

fn forget(cookies: &Cookies) {
	cookies.remove(Cookie::build((PENDING, "")).path(RETURN).build());
}

/// `/invite`: the invite remembered in the cookie, after signing in.
async fn pending(State(s): State<AppState>, cookies: Cookies, headers: HeaderMap) -> Response {
	match cookies.get(PENDING).map(|c| c.value().to_owned()) {
		Some(token) if invites::well_formed(&token) => show(&s, &cookies, &headers, &token).await,
		_ => invalid_page(),
	}
}

fn invalid_page() -> Response {
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "This invite is not valid any more" }
			p class="ui-lede" { "It may have been used already, revoked, or it expired. Ask whoever invited you to send a new one." }
		}
		a class={ (BUTTON) " ui-button-lg ui-button-block" } href="/login" {
			(icon("arrow-left"))
			span { "Go to sign in" }
		}
	};
	private(
		(
			StatusCode::NOT_FOUND,
			layout::auth_page("Invite not valid", None, &card, None),
		)
			.into_response(),
	)
}

fn sign_up_form(token: &str, p: &Preview) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}')", invites::path(token));
	html! {
		form class="ui-auth-form" "data-on:submit"=(post) {
			div class="ui-field-group" {
				label class=(LABEL) for="inv-email" { "Email" }
				input id="inv-email" class=(INPUT) type="email" autocomplete="username" value=(p.email) readonly;
				p class="ui-hint" { "The invite is for this address; your account will use it." }
			}
			div class="ui-field-group" {
				label class=(LABEL) for="inv-name" { "Name" }
				input id="inv-name" class=(INPUT) type="text" autocomplete="name" required autofocus maxlength="100" "data-bind:name";
			}
			div class="ui-field-group" {
				label class=(LABEL) for="inv-pw" { "Password" }
				div class="ui-pw" {
					input id="inv-pw" class=(INPUT) type="password" autocomplete="new-password" required minlength="8" "data-bind:password";
					(reveal_button())
				}
				p class="ui-hint" { "At least 8 characters." }
			}
			div class="ui-field-group" {
				label class=(LABEL) for="inv-confirm" { "Confirm password" }
				div class="ui-pw" {
					input id="inv-confirm" class=(INPUT) type="password" autocomplete="new-password" required minlength="8" "data-bind:confirm";
					(reveal_button())
				}
			}
			(form_banner())
			button type="submit" class={ (BUTTON) " ui-button-lg ui-button-block" } "data-indicator:_busy" "data-attr:disabled"="$_busy" "data-attr:aria-busy"="$_busy" {
				span { "Create account and join" }
				(icon("arrow-right"))
			}
		}
	}
}

fn accept_form(token: &str, p: &Preview) -> Markup {
	let post = format!("@post('{}/accept')", invites::path(token));
	let login = format!("/login?next={RETURN}");
	html! {
		div class="ui-auth-form" {
			p class="ui-lede" { (p.email) " already has a podkit account. Sign in with it, then accept." }
			(form_banner())
			button type="button" class={ (BUTTON) " ui-button-lg ui-button-block" } "data-on:click"=(post) "data-indicator:_busy" "data-attr:disabled"="$_busy" "data-attr:aria-busy"="$_busy" {
				span { "Accept invite" }
				(icon("arrow-right"))
			}
		}
		p class="ui-auth-foot" { "Not signed in? " a href=(login) { "Sign in as " (p.email) } }
	}
}

async fn page(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Path(token): Path<String>,
) -> Response {
	show(&s, &cookies, &headers, &token).await
}

async fn show(s: &AppState, cookies: &Cookies, headers: &HeaderMap, token: &str) -> Response {
	let p = match invites::preview(&s.db, token).await {
		Ok(p) => p,
		Err(ServerError::InviteNotFound) => {
			forget(cookies);
			return invalid_page();
		}
		Err(e) => return private(error::bare(None, &e)),
	};
	if p.account_exists {
		remember(cookies, headers, token);
	}
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Join " (p.team_name) }
			p class="ui-lede" {
				@if let Some(by) = &p.invited_by { (by) } @else { "Someone" }
				" invited " b { (p.email) } " to this team as " b { (p.role_name) } ". "
				"The link works once and expires on " (p.expires_at.date()) " UTC."
			}
		}
		@if p.account_exists {
			(accept_form(token, &p))
		} @else {
			(sign_up_form(token, &p))
		}
	};
	private(
		layout::auth_page(
			"Accept an invite",
			Some("{name: '', password: '', confirm: '', _error: ''}"),
			&card,
			None,
		)
		.into_response(),
	)
}

#[derive(Deserialize)]
struct SignUpSignals {
	name: String,
	password: String,
	confirm: String,
}

fn refusal(message: &str) -> Response {
	sse::signals(&json!({ "_error": message, "password": "", "confirm": "" })).into_response()
}

/// Creates the invited account, joins the team and signs in.
async fn sign_up(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Path(token): Path<String>,
	ReadSignals(sig): ReadSignals<SignUpSignals>,
) -> Response {
	if sig.password != sig.confirm {
		return refusal("The passwords don't match.");
	}
	let (user, team) = match invites::sign_up(&s.db, &token, &sig.name, None, &sig.password).await {
		Ok(made) => made,
		Err(e @ (ServerError::InviteNotFound | ServerError::EmailTaken)) => {
			return refusal(&e.to_string());
		}
		Err(e) => return refusal(&error::message(&e)),
	};
	forget(&cookies);
	match auth::open_session(&s, &cookies, &headers, user, session::Method::Password).await {
		Ok(_) => sse::redirect(&format!("/teams/{team}/projects")).into_response(),
		Err(_) => sse::redirect("/login").into_response(),
	}
}

/// Joins the team with the signed-in account; someone signed out is sent to sign in and
/// comes back here.
async fn accept(
	State(s): State<AppState>,
	cookies: Cookies,
	Path(token): Path<String>,
	session_token: Result<SessionToken, ServerError>,
) -> Response {
	if !invites::well_formed(&token) {
		return refusal(&ServerError::InviteNotFound.to_string());
	}
	let user = match session_token {
		Ok(SessionToken(t)) if !t.starts_with(auth::tokens::PREFIX) => {
			session::resolve(&s.db, &t).await.ok().flatten()
		}
		_ => None,
	};
	let Some(user) = user else {
		// The page already left the token in the cookie; sign-in returns to plain `/invite`.
		return sse::redirect(&format!("/login?next={RETURN}")).into_response();
	};
	match invites::accept(&s.db, user, &token).await {
		Ok(team) => {
			forget(&cookies);
			sse::redirect(&format!("/teams/{team}/projects")).into_response()
		}
		Err(e @ ServerError::InviteNotFound) => refusal(&e.to_string()),
		Err(e) => refusal(&error::message(&e)),
	}
}

#[cfg(test)]
mod tests;
