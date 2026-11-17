//! Server-rendered web UI. Pages are maud templates; Datastar adds
//! interactivity over SSE. Requests authenticate with the `session` cookie
//! that login sets, so the UI needs no CORS and holds no token in the browser.

use std::convert::Infallible;

use axum::{
	Router,
	extract::State,
	http::header,
	response::{IntoResponse, Redirect, Response, Sse, sse::Event},
	routing::{get, post},
};
use datastar::{axum::ReadSignals, prelude::*};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use tower_cookies::Cookies;

use crate::auth::{self, AuthUser, Credentials, SessionToken};
use crate::error::ServerError;
use crate::state::AppState;
use crate::teams;

/// Versioned so browsers can cache it forever; bump with the vendored file.
const DATASTAR_PATH: &str = "/static/datastar-1.0.5.js";
const DATASTAR_JS: &str = include_str!("../../static/datastar-1.0.5.js");

const CSS: &str = "
body{font:15px/1.5 system-ui,sans-serif;margin:0;color:#1d1d1f;background:#f6f6f7}
header{display:flex;justify-content:space-between;align-items:center;padding:.75rem 1.5rem;background:#fff;border-bottom:1px solid #e3e3e6}
main{max-width:48rem;margin:2rem auto;padding:0 1.5rem}
.auth{max-width:22rem}
label{display:block;margin-bottom:1rem}
input{display:block;width:100%;box-sizing:border-box;padding:.5rem;margin-top:.25rem;font:inherit}
button{font:inherit;padding:.45rem 1rem;cursor:pointer}
.error{color:#b42318}
ul.cards{list-style:none;padding:0}
ul.cards li{background:#fff;border:1px solid #e3e3e6;border-radius:6px;padding:.75rem 1rem;margin-bottom:.5rem}
.muted{color:#6e6e73}
";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(index))
		.route("/login", get(login_page).post(login))
		.route("/logout", post(logout))
		.route(DATASTAR_PATH, get(datastar_js))
}

fn layout(title: &str, body: &Markup) -> Markup {
	html! {
		(DOCTYPE)
		html lang="en" {
			head {
				meta charset="utf-8";
				meta name="viewport" content="width=device-width, initial-scale=1";
				title { (title) " · podkit" }
				script type="module" src=(DATASTAR_PATH) {}
				style { (PreEscaped(CSS)) }
			}
			body { (body) }
		}
	}
}

async fn datastar_js() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
		],
		DATASTAR_JS,
	)
}

async fn index(State(s): State<AppState>, user: Result<AuthUser, ServerError>) -> Response {
	let user = match user {
		Ok(user) => user,
		Err(ServerError::MissingToken | ServerError::InvalidToken) => {
			return Redirect::to("/login").into_response();
		}
		Err(e) => return e.into_response(),
	};
	let teams = match teams::list_for_user(&s.db, user.id).await {
		Ok(teams) => teams,
		Err(e) => return ServerError::from(e).into_response(),
	};

	layout(
		"Teams",
		&html! {
			header {
				strong { "podkit" }
				form method="post" action="/logout" { button type="submit" { "Sign out" } }
			}
			main {
				h1 { "Teams" }
				@if teams.is_empty() {
					p.muted { "You are not a member of any team yet." }
				} @else {
					ul.cards {
						@for team in &teams {
							li { strong { (team.name) } " " span.muted { (team.slug) } }
						}
					}
				}
			}
		},
	)
	.into_response()
}

async fn login_page() -> Markup {
	layout(
		"Sign in",
		&html! {
			main.auth data-signals="{email: '', password: '', _error: ''}" {
				h1 { "Sign in to podkit" }
				form "data-on:submit"="evt.preventDefault(); @post('/login')" {
					label {
						"Email"
						input type="email" autocomplete="username" required "data-bind:email";
					}
					label {
						"Password"
						input type="password" autocomplete="current-password" required "data-bind:password";
					}
					p.error role="alert" "data-show"="$_error" "data-text"="$_error" {}
					// Browsers drop the `Secure` session cookie outside a secure context,
					// which would bounce a "successful" sign-in back to this page.
					p.error role="alert" "data-show"="!window.isSecureContext" style="display: none" {
						"Sign-in needs HTTPS. Open podkit over https:// or on localhost."
					}
					button type="submit" "data-indicator:_busy" "data-attr:disabled"="$_busy || !window.isSecureContext" { "Sign in" }
				}
			}
		},
	)
}

fn one_event(event: Event) -> Response {
	Sse::new(futures_util::stream::once(async move {
		Ok::<_, Infallible>(event)
	}))
	.into_response()
}

async fn login(
	State(s): State<AppState>,
	cookies: Cookies,
	ReadSignals(credentials): ReadSignals<Credentials>,
) -> Response {
	let message = match auth::log_in(&s, &cookies, credentials).await {
		Ok(_) => {
			return one_event(
				ExecuteScript::new("window.location.assign('/')").write_as_axum_sse_event(),
			);
		}
		Err(ServerError::InvalidCredentials) => "Wrong email or password.",
		Err(e) => {
			tracing::error!(error = %e, "ui login failed");
			"Sign-in failed. Try again."
		}
	};
	let signals = serde_json::json!({ "_error": message, "password": "" });
	one_event(PatchSignals::new(signals.to_string()).write_as_axum_sse_event())
}

async fn logout(
	State(s): State<AppState>,
	cookies: Cookies,
	token: Result<SessionToken, ServerError>,
) -> Response {
	if let Ok(SessionToken(token)) = token
		&& let Err(e) = auth::log_out(&s, &cookies, &token).await
	{
		return e.into_response();
	}
	Redirect::to("/login").into_response()
}

#[cfg(test)]
mod tests {
	use axum::body::Body;
	use axum::http::{HeaderMap, Request, StatusCode, header};
	use sqlx::PgPool;
	use tower::ServiceExt;

	use super::Router;
	use crate::tests::{app, user_with_team};

	async fn send(
		app: &Router,
		method: &str,
		uri: &str,
		cookie: Option<&str>,
		signals: Option<&str>,
	) -> (StatusCode, HeaderMap, String) {
		let mut req = Request::builder().method(method).uri(uri);
		if let Some(c) = cookie {
			req = req.header(header::COOKIE, c);
		}
		let body = match signals {
			Some(s) => {
				req = req
					.header(header::CONTENT_TYPE, "application/json")
					.header("datastar-request", "true");
				Body::from(s.to_owned())
			}
			None => Body::empty(),
		};
		let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
		let (parts, body) = resp.into_parts();
		let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
		(
			parts.status,
			parts.headers,
			String::from_utf8(bytes.to_vec()).unwrap(),
		)
	}

	#[sqlx::test]
	async fn cookie_session_signs_in_lists_teams_and_signs_out(db: PgPool) {
		let app = app(db).await;
		user_with_team(&app, "alice").await;

		let (s, h, _) = send(&app, "GET", "/", None, None).await;
		assert_eq!(s, StatusCode::SEE_OTHER);
		assert_eq!(h[header::LOCATION], "/login");

		let wrong = r#"{"email":"alice@example.com","password":"nope"}"#;
		let (s, h, body) = send(&app, "POST", "/login", None, Some(wrong)).await;
		assert_eq!(s, StatusCode::OK);
		assert!(h.get(header::SET_COOKIE).is_none());
		assert!(body.contains("Wrong email or password."), "{body}");

		let right = r#"{"email":"alice@example.com","password":"correct horse battery"}"#;
		let (s, h, body) = send(&app, "POST", "/login", None, Some(right)).await;
		assert_eq!(s, StatusCode::OK);
		assert!(body.contains("window.location.assign('/')"), "{body}");
		let set_cookie = h[header::SET_COOKIE].to_str().unwrap();
		assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
		let cookie = set_cookie.split(';').next().unwrap().to_owned();

		let (s, _, body) = send(&app, "GET", "/", Some(&cookie), None).await;
		assert_eq!(s, StatusCode::OK);
		assert!(body.contains("<strong>alice</strong>"), "{body}");

		let (s, h, _) = send(&app, "POST", "/logout", Some(&cookie), None).await;
		assert_eq!(s, StatusCode::SEE_OTHER);
		assert_eq!(h[header::LOCATION], "/login");

		let (s, _, _) = send(&app, "GET", "/", Some(&cookie), None).await;
		assert_eq!(
			s,
			StatusCode::SEE_OTHER,
			"revoked session must not render the page"
		);
	}
}
