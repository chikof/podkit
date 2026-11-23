//! Sign-up page. Whether it works depends on the instance's registration
//! policy; the shared `auth::service::register` decides.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;
use tower_cookies::Cookies;

use crate::auth::{self, Credentials, service};
use crate::error::ServerError;
use crate::state::AppState;
use crate::ui::components::{BUTTON, INPUT, LABEL, icon, reveal_button};
use crate::ui::layout::{self, form_banner};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new().route("/register", get(page).post(register))
}

fn closed_page() -> Markup {
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Registration is closed" }
			p class="ui-lede" { "This podkit instance doesn't accept new sign-ups. Ask an administrator to add you." }
		}
		a class={ (BUTTON) " ui-button-lg ui-button-block" } href="/login" {
			(icon("arrow-left"))
			span { "Back to sign in" }
		}
	};
	layout::auth_page("Registration is closed", None, &card, None)
}

async fn page(State(s): State<AppState>) -> Response {
	match service::registration_allowed(&s.db, s.config.registration).await {
		Ok(true) => {}
		Ok(false) => return (StatusCode::FORBIDDEN, closed_page()).into_response(),
		Err(e) => return error::bare(None, &e),
	}
	let card = html! {
		div class="ui-auth-head" {
			h1 id="auth-t" { "Create your account" }
			p class="ui-lede" { "On a new install the first account becomes the administrator." }
		}
		form class="ui-auth-form" "data-on:submit"="evt.preventDefault(); @post('/register')" {
			div class="ui-field-group" {
				label class=(LABEL) for="su-name" { "Name" }
				input id="su-name" class=(INPUT) type="text" autocomplete="name" required autofocus maxlength="100" "data-bind:name";
			}
			div class="ui-field-group" {
				label class=(LABEL) for="su-email" { "Email" }
				input id="su-email" class=(INPUT) type="email" autocomplete="username" required placeholder="you@example.com" "data-bind:email";
			}
			div class="ui-field-group" {
				label class=(LABEL) for="su-pw" { "Password" }
				div class="ui-pw" {
					input id="su-pw" class=(INPUT) type="password" autocomplete="new-password" required minlength="8" "data-bind:password";
					(reveal_button())
				}
				p class="ui-hint" { "At least 8 characters." }
			}
			div class="ui-field-group" {
				label class=(LABEL) for="su-confirm" { "Confirm password" }
				div class="ui-pw" {
					input id="su-confirm" class=(INPUT) type="password" autocomplete="new-password" required minlength="8" "data-bind:confirm";
					(reveal_button())
				}
			}
			(form_banner())
			p class="ui-auth-foot" "data-show"="!window.isSecureContext" style="display: none" {
				"This connection isn't encrypted, so your password and session travel in plain text. Use HTTPS when podkit is reachable from other machines."
			}
			button type="submit" class={ (BUTTON) " ui-button-lg ui-button-block" } "data-indicator:_busy" "data-attr:disabled"="$_busy" "data-attr:aria-busy"="$_busy" {
				span { "Create account" }
				(icon("arrow-right"))
			}
		}
		p class="ui-auth-foot" { "Already have an account? " a href="/login" { "Sign in" } }
	};
	layout::auth_page(
		"Create an account",
		Some("{name: '', email: '', password: '', confirm: '', _error: ''}"),
		&card,
		Some(&layout::auth_meta()),
	)
	.into_response()
}

#[derive(Deserialize)]
struct RegisterSignals {
	name: String,
	email: String,
	password: String,
	confirm: String,
}

/// Refused forms keep what was typed except the passwords.
fn refusal(message: &str) -> Response {
	sse::signals(&json!({ "_error": message, "password": "", "confirm": "" })).into_response()
}

async fn register(
	State(s): State<AppState>,
	cookies: Cookies,
	headers: HeaderMap,
	ReadSignals(sig): ReadSignals<RegisterSignals>,
) -> Response {
	if sig.password != sig.confirm {
		return refusal("The passwords don't match.");
	}
	let created = service::register(
		&s.db,
		s.config.registration,
		&sig.name,
		&sig.email,
		&sig.password,
	)
	.await;
	if let Err(e) = created {
		return refusal(&error::message(&e));
	}

	let credentials = Credentials {
		email: auth::normalize_email_lenient(&sig.email),
		password: sig.password,
		code: None,
	};
	match auth::log_in(&s, &cookies, &headers, credentials).await {
		Ok(_) => sse::redirect("/teams/new").into_response(),
		Err(ServerError::InvalidCredentials) => sse::redirect("/login").into_response(),
		Err(e) => refusal(&error::message(&e)),
	}
}

#[cfg(test)]
mod tests {
	use axum::http::{StatusCode, header};
	use serde_json::json;

	use crate::state::Registration;
	use crate::tests::{PgPool, app_with_registration, call, count, db_test};
	use crate::ui::testing::{assert_contains, assert_not_contains, get_html, post_signals};

	fn form(name: &str, email: &str) -> serde_json::Value {
		json!({
			"name": name, "email": email,
			"password": "correct horse battery", "confirm": "correct horse battery",
		})
	}

	fn api_body(email: &str) -> serde_json::Value {
		json!({ "name": "x", "email": email, "password": "correct horse battery" })
	}

	#[db_test]
	async fn registration_first_user_via_ui_signs_in_and_goes_to_create_team(db: PgPool) {
		let app = app_with_registration(db.clone(), Registration::FirstUser).await;

		let (s, _, page) = get_html(&app, "/register", None).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Confirm password");

		let (s, h, body) =
			post_signals(&app, "/register", None, &form("Alice", "alice@example.com")).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "window.location.assign('/teams/new')");
		let cookie = h[header::SET_COOKIE]
			.to_str()
			.unwrap()
			.split(';')
			.next()
			.unwrap()
			.to_owned();
		assert_eq!(count(&db, "users").await, 1);

		let (s, _, page) = get_html(&app, "/teams/new", Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Create a team");
		assert_contains(&page, "Alice");
	}

	#[db_test]
	async fn registration_first_user_refuses_the_second_via_api_and_ui(db: PgPool) {
		let app = app_with_registration(db.clone(), Registration::FirstUser).await;

		let (s, _) = call(
			&app,
			"POST",
			"/api/v1/auth/register",
			None,
			Some(api_body("a@example.com")),
		)
		.await;
		assert_eq!(s, StatusCode::CREATED);

		let (s, body) = call(
			&app,
			"POST",
			"/api/v1/auth/register",
			None,
			Some(api_body("x2@example.com")),
		)
		.await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		assert_eq!(body["error"], "registration is closed");

		let (s, _, body) =
			post_signals(&app, "/register", None, &form("Bob", "bob@example.com")).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "registration is closed");
		assert_eq!(count(&db, "users").await, 1);

		let (s, _, page) = get_html(&app, "/register", None).await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		assert_contains(&page, "Registration is closed");
		assert_not_contains(&page, "Confirm password");

		let (_, _, login) = get_html(&app, "/login", None).await;
		assert_not_contains(&login, "href=\"/register\"");
	}

	#[db_test]
	async fn registration_open_allows_any_number(db: PgPool) {
		let app = app_with_registration(db.clone(), Registration::Open).await;
		for i in 0..3 {
			let email = format!("u{i}@example.com");
			let (s, _) = call(
				&app,
				"POST",
				"/api/v1/auth/register",
				None,
				Some(api_body(&email)),
			)
			.await;
			assert_eq!(s, StatusCode::CREATED, "{email}");
		}
		let (s, _, body) =
			post_signals(&app, "/register", None, &form("Dee", "dee@example.com")).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "window.location.assign('/teams/new')");
		assert_eq!(count(&db, "users").await, 4);

		let (_, _, login) = get_html(&app, "/login", None).await;
		assert_contains(&login, "href=\"/register\"");
	}

	#[db_test]
	async fn registration_closed_allows_none(db: PgPool) {
		let app = app_with_registration(db.clone(), Registration::Closed).await;
		let (s, body) = call(
			&app,
			"POST",
			"/api/v1/auth/register",
			None,
			Some(api_body("a@example.com")),
		)
		.await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		assert_eq!(body["error"], "registration is closed");

		let (_, _, body) =
			post_signals(&app, "/register", None, &form("Al", "al@example.com")).await;
		assert_contains(&body, "registration is closed");
		assert_eq!(count(&db, "users").await, 0);

		let (s, _, _) = get_html(&app, "/register", None).await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		let (_, _, login) = get_html(&app, "/login", None).await;
		assert_not_contains(&login, "href=\"/register\"");
	}

	#[db_test]
	async fn registration_rejects_mismatched_weak_and_duplicate_input(db: PgPool) {
		let app = app_with_registration(db.clone(), Registration::Open).await;

		let mut f = form("Al", "al@example.com");
		f["confirm"] = json!("something else entirely");
		let (_, _, body) = post_signals(&app, "/register", None, &f).await;
		assert_contains(&body, "The passwords don't match.");
		// A refused form clears both password fields, the confirmation too.
		assert_contains(&body, "\"password\":\"\"");
		assert_contains(&body, "\"confirm\":\"\"");

		let mut f = form("Al", "al@example.com");
		f["password"] = json!("short");
		f["confirm"] = json!("short");
		let (_, _, body) = post_signals(&app, "/register", None, &f).await;
		assert_contains(&body, "Password must be at least 8 characters.");

		let (_, _, body) = post_signals(&app, "/register", None, &form("Al", "not-an-email")).await;
		assert_contains(&body, "_error");
		assert_eq!(count(&db, "users").await, 0);

		post_signals(&app, "/register", None, &form("Al", "al@example.com")).await;
		let (_, _, body) =
			post_signals(&app, "/register", None, &form("Al2", "al@example.com")).await;
		assert_contains(&body, "an account with this email already exists");
		assert_eq!(count(&db, "users").await, 1);
	}

	#[db_test]
	async fn registration_name_is_escaped_where_it_is_shown(db: PgPool) {
		let app = app_with_registration(db.clone(), Registration::Open).await;
		let evil = "<script>alert(1)</script>";
		let (_, h, _) =
			post_signals(&app, "/register", None, &form(evil, "evil@example.com")).await;
		let cookie = h[header::SET_COOKIE]
			.to_str()
			.unwrap()
			.split(';')
			.next()
			.unwrap()
			.to_owned();
		let (_, _, page) = get_html(&app, "/teams/new", Some(&cookie)).await;
		assert_not_contains(&page, evil);
		assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}
}
