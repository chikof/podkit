//! The pages of a sign-in that are not the password form: the second step (a code or a
//! passkey), a passkey sign-in with no password, "Sign in with <provider>" and its callback,
//! and the page that asks for a password before an existing account is linked to a provider.
//!
//! None of it is inside a team's frame: the person has no session yet. The handlers only call
//! `two_factor`, `sso` and `auth` services.

use axum::Router;
use axum::extract::{Json, Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_cookies::Cookies;

use crate::assets::passkey_script;
use crate::components::{BUTTON, INPUT, LABEL, icon, reveal_button};
use crate::layout::{self, form_banner};
use crate::{error, safe_next, sse};
use podkit_core::auth::origin::external_base;
use podkit_core::error::ServerError;
use podkit_core::ids::AuthProviderId;
use podkit_core::sso::signin::{self, Callback, Landing, Linked};
use podkit_core::state::AppState;
use podkit_core::two_factor::challenge;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/login/two-step", get(two_step_page).post(two_step_submit))
		.route(
			"/login/two-step/passkey/start",
			post(two_step_passkey_start),
		)
		.route(
			"/login/two-step/passkey/finish",
			post(two_step_passkey_finish),
		)
		.route("/login/passkey/start", post(passkey_start))
		.route("/login/passkey/finish", post(passkey_finish))
		.route("/sso/{provider_id}/start", get(sso_start))
		.route("/sso/{provider_id}/callback", get(sso_callback))
		.route("/sso/link", get(link_page).post(link_submit))
}

/// A JSON answer for the passkey script: the message is for the person.
fn json_error(e: &ServerError) -> Response {
	(e.status(), Json(json!({ "error": error::message(e) }))).into_response()
}

/// A page that sends the browser on at once. Not a redirect: the cookies a provider's
/// callback sets are `SameSite=Strict`, and a browser does not send them on a redirect that
/// follows a navigation from another site. A navigation the page itself starts is
/// same-site, so they go along.
fn continue_to(url: &str) -> Response {
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Signing you in" }
			p class="ui-lede" { "One moment. If nothing happens, " a href=(url) { "continue" } "." }
		}
	};
	let mut response = layout::auth_page("Signing you in", None, &card, None).into_response();
	if let Ok(value) = header::HeaderValue::from_str(&format!("0; url={url}")) {
		response.headers_mut().insert(header::REFRESH, value);
	}
	response
}

// --- the second step -------------------------------------------------------------

#[derive(Deserialize)]
struct StepSignals {
	#[serde(default)]
	code: String,
	#[serde(default)]
	recovery: String,
}

fn step_card(totp: bool, passkeys: bool) -> Markup {
	html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Two-step verification" }
			p class="ui-lede" {
				@if totp { "Enter the 6-digit code from your authenticator app." }
				@else { "Use a passkey to finish signing in." }
			}
		}
		@if totp {
			form class="ui-auth-form" "data-on:submit"="evt.preventDefault(); @post('/login/two-step')" {
				div class="ui-field-group" {
					label class=(LABEL) for="tf-code" { "Code" }
					p id="tf-code-hint" class="ui-hint" { "Six digits. It changes every 30 seconds." }
					input id="tf-code" class=(INPUT) type="text" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 ]{6,7}" maxlength="7" required autofocus aria-describedby="tf-code-hint" "data-bind:code";
				}
				(form_banner())
				button type="submit" class={ (BUTTON) " ui-button-lg ui-button-block" } "data-indicator:_busy" "data-attr:disabled"="$_busy" "data-attr:aria-busy"="$_busy" {
					span { "Verify" }
					(icon("arrow-right"))
				}
			}
			details class="ui-auth-alt" {
				summary { "Use a recovery code instead" }
				form class="ui-auth-form" "data-on:submit"="evt.preventDefault(); @post('/login/two-step')" {
					div class="ui-field-group" {
						label class=(LABEL) for="tf-recovery" { "Recovery code" }
						p id="tf-recovery-hint" class="ui-hint" { "One of the codes you saved. Each works once." }
						input id="tf-recovery" class=(INPUT) type="text" autocomplete="off" autocapitalize="none" spellcheck="false" maxlength="24" required aria-describedby="tf-recovery-hint" "data-bind:recovery";
					}
					button type="submit" class={ (BUTTON) " ui-button-lg ui-button-block" } "data-indicator:_busy" "data-attr:disabled"="$_busy" { span { "Use recovery code" } }
				}
			}
		}
		@if passkeys {
			div class="ui-auth-form" {
				button type="button" class={ "ui-button ui-button-outline ui-button-lg ui-button-block" } data-passkey="second" hidden {
					(icon("key-round"))
					span { "Use a passkey" }
				}
				p class="ui-hint" data-passkey-status role="status" {}
				p class="ui-hint" data-passkey-note hidden { "Passkeys need a secure connection (HTTPS) and a browser that supports them." }
			}
		}
		p class="ui-auth-foot" { a href="/login" { "Start over" } }
	}
}

async fn two_step_page(State(s): State<AppState>, cookies: Cookies) -> Response {
	let pending = match challenge::pending(&s, &cookies).await {
		Ok(Some(p)) => p,
		Ok(None) => return Redirect::to("/login").into_response(),
		Err(e) => return error::bare(None, &e),
	};
	let card = html! {
		(step_card(pending.totp, pending.passkeys))
		@if pending.passkeys { (passkey_script()) }
	};
	let signals = "{code: '', recovery: '', _error: ''}";
	layout::auth_page(
		"Two-step verification",
		Some(signals),
		&card,
		Some(&layout::auth_meta()),
	)
	.into_response()
}

async fn two_step_submit(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	ReadSignals(sig): ReadSignals<StepSignals>,
) -> Response {
	let code = if sig.code.trim().is_empty() {
		sig.recovery
	} else {
		sig.code
	};
	match challenge::submit_code(&s, &cookies, &headers, &code).await {
		Ok(next) => sse::redirect(safe_next(&next)).into_response(),
		Err(ServerError::InvalidCredentials) => sse::signals(&json!({
			"_error": "That code is not right.", "code": "", "recovery": "",
		}))
		.into_response(),
		Err(e) => {
			sse::signals(&json!({ "_error": error::message(&e), "code": "", "recovery": "" }))
				.into_response()
		}
	}
}

async fn two_step_passkey_start(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
) -> Response {
	let origin = external_base(&s.config, &headers);
	match challenge::passkey_options(&s, &cookies, &origin).await {
		Ok(c) => Json(json!({ "flow": c.flow, "options": c.options })).into_response(),
		Err(e) => json_error(&e),
	}
}

#[derive(Deserialize)]
struct Finish {
	flow: String,
	credential: Value,
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ServerError> {
	serde_json::from_value(value)
		.map_err(|_| ServerError::Validation("The passkey answer could not be read.".to_owned()))
}

async fn two_step_passkey_finish(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Json(body): Json<Finish>,
) -> Response {
	let origin = external_base(&s.config, &headers);
	let done = async {
		let credential = parse(body.credential)?;
		challenge::submit_passkey(&s, &cookies, &headers, &origin, &body.flow, &credential).await
	}
	.await;
	match done {
		Ok(next) => Json(json!({ "next": safe_next(&next) })).into_response(),
		Err(e) => json_error(&e),
	}
}

// --- a passkey and nothing else --------------------------------------------------

#[derive(Deserialize)]
struct EmailBody {
	email: String,
}

async fn passkey_start(
	State(s): State<AppState>,
	headers: HeaderMap,
	Json(body): Json<EmailBody>,
) -> Response {
	let origin = external_base(&s.config, &headers);
	match challenge::passwordless_options(&s, &origin, &body.email).await {
		Ok(Some(c)) => Json(json!({ "flow": c.flow, "options": c.options })).into_response(),
		// An unknown email and an email with no passkey look the same.
		Ok(None) => json_error(&ServerError::Validation(
			"No passkey is registered for that email.".to_owned(),
		)),
		Err(e) => json_error(&e),
	}
}

async fn passkey_finish(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Json(body): Json<Finish>,
) -> Response {
	let origin = external_base(&s.config, &headers);
	let done = async {
		let credential = parse(body.credential)?;
		challenge::passwordless_finish(&s, &cookies, &headers, &origin, &body.flow, &credential)
			.await
	}
	.await;
	match done {
		Ok(()) => Json(json!({ "next": "/" })).into_response(),
		Err(e) => json_error(&e),
	}
}

// --- an identity provider --------------------------------------------------------

#[derive(Deserialize)]
struct StartQuery {
	next: Option<String>,
}

async fn sso_start(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Path(id): Path<AuthProviderId>,
	Query(q): Query<StartQuery>,
) -> Response {
	let next = safe_next(q.next.as_deref().unwrap_or("/"));
	match signin::begin(&s, &cookies, &headers, id, next).await {
		Ok(url) => Redirect::to(&url).into_response(),
		Err(e) => error::bare(None, &e),
	}
}

#[derive(Deserialize)]
struct CallbackQuery {
	code: Option<String>,
	state: Option<String>,
	error: Option<String>,
}

async fn sso_callback(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	Path(id): Path<AuthProviderId>,
	Query(q): Query<CallbackQuery>,
) -> Response {
	let answer = Callback {
		code: q.code,
		state: q.state,
		error: q.error,
	};
	match signin::callback(&s, &cookies, &headers, id, answer).await {
		Ok(Landing::SignedIn(next)) => continue_to(safe_next(&next)),
		Ok(Landing::LinkNeeded) => continue_to("/sso/link"),
		Err(e) => sign_in_failed(&e),
	}
}

/// What a person sees when the round trip fails: the reason, and the way back.
fn sign_in_failed(e: &ServerError) -> Response {
	let message = error::message(e);
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Sign-in did not work" }
			p class="ui-lede" { (message) }
		}
		a class={ (BUTTON) " ui-button-lg ui-button-block" } href="/login" {
			(icon("arrow-left"))
			span { "Back to sign in" }
		}
	};
	(
		e.status(),
		layout::auth_page("Sign-in did not work", None, &card, None),
	)
		.into_response()
}

#[derive(Deserialize)]
struct LinkSignals {
	#[serde(default)]
	password: String,
}

async fn link_page(State(s): State<AppState>, cookies: Cookies) -> Response {
	let prompt = match signin::pending_link(&s, &cookies).await {
		Ok(Some(p)) => p,
		Ok(None) => return Redirect::to("/login").into_response(),
		Err(e) => return error::bare(None, &e),
	};
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Link your account" }
			p class="ui-lede" {
				"A podkit account already uses "
				@if let Some(email) = &prompt.email { strong { (email) } } @else { "this email" }
				". Enter its password to link " strong { (prompt.provider) } " to it. You only do this once."
			}
		}
		form class="ui-auth-form" "data-on:submit"="evt.preventDefault(); @post('/sso/link')" {
			div class="ui-field-group" {
				label class=(LABEL) for="lk-pw" { "Password" }
				div class="ui-pw" {
					input id="lk-pw" class=(INPUT) type="password" autocomplete="current-password" required autofocus "data-bind:password";
					(reveal_button())
				}
			}
			(form_banner())
			button type="submit" class={ (BUTTON) " ui-button-lg ui-button-block" } "data-indicator:_busy" "data-attr:disabled"="$_busy" "data-attr:aria-busy"="$_busy" {
				span { "Link and sign in" }
				(icon("arrow-right"))
			}
		}
		p class="ui-auth-foot" { a href="/login" { "Cancel" } }
	};
	layout::auth_page(
		"Link your account",
		Some("{password: '', _error: ''}"),
		&card,
		Some(&layout::auth_meta()),
	)
	.into_response()
}

async fn link_submit(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	ReadSignals(sig): ReadSignals<LinkSignals>,
) -> Response {
	match signin::finish_link(&s, &cookies, &headers, &sig.password).await {
		Ok(Linked::SignedIn(next)) => sse::redirect(safe_next(&next)).into_response(),
		Ok(Linked::SecondFactor) => sse::redirect("/login/two-step").into_response(),
		Err(ServerError::InvalidCredentials) => {
			sse::signals(&json!({ "_error": "Wrong password.", "password": "" })).into_response()
		}
		Err(e) => {
			sse::signals(&json!({ "_error": error::message(&e), "password": "" })).into_response()
		}
	}
}
