//! Test helpers for the web UI: a signed-in cookie session, GETs that return
//! HTML, Datastar-style signal POSTs and body assertions.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::tests::user_with_team;

/// The `Host` every test request carries, and the matching `Origin`.
pub const HOST: &str = "podkit.test";

pub async fn send(
	app: &Router,
	method: &str,
	uri: &str,
	cookie: Option<&str>,
	signals: Option<&str>,
	origin: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
	let mut req = Request::builder()
		.method(method)
		.uri(uri)
		.header(header::HOST, HOST);
	if let Some(c) = cookie {
		req = req.header(header::COOKIE, c);
	}
	if let Some(o) = origin {
		req = req.header(header::ORIGIN, o);
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

/// Registers `name`, creates their team through the API, then signs in
/// through the UI. Returns the `session=...` cookie and the team id.
pub async fn ui_session(app: &Router, name: &str) -> (String, i64) {
	let (_token, team) = user_with_team(app, name).await;
	(ui_login(app, name).await, team)
}

/// Signs `name@example.com` in through the UI and returns the session cookie.
pub async fn ui_login(app: &Router, name: &str) -> String {
	let creds =
		json!({ "email": format!("{name}@example.com"), "password": "correct horse battery" });
	let (s, headers, _) = post_signals(app, "/login", None, &creds).await;
	assert_eq!(s, StatusCode::OK, "ui login {name}");
	let set_cookie = headers[header::SET_COOKIE].to_str().unwrap();
	set_cookie.split(';').next().unwrap().to_owned()
}

pub async fn get_html(
	app: &Router,
	uri: &str,
	cookie: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
	send(app, "GET", uri, cookie, None, None).await
}

/// A Datastar action: POST with the signals as JSON and a same-origin `Origin`.
pub async fn post_signals(
	app: &Router,
	uri: &str,
	cookie: Option<&str>,
	signals: &Value,
) -> (StatusCode, HeaderMap, String) {
	let origin = format!("http://{HOST}");
	send(
		app,
		"POST",
		uri,
		cookie,
		Some(&signals.to_string()),
		Some(&origin),
	)
	.await
}

/// The page's own content: what is inside `<main id="main">`, without the sidebar,
/// header and palette every team page carries. Use it to assert that a control is absent
/// from a page, since "Settings" or "Members" also name sidebar links.
#[track_caller]
pub fn page_main(body: &str) -> &str {
	let start = body
		.find("<main id=\"main\"")
		.unwrap_or_else(|| panic!("no <main id=\"main\"> in:\n{body}"));
	let rest = &body[start..];
	rest.find("</main>").map_or(rest, |end| &rest[..end])
}

#[track_caller]
pub fn assert_contains(body: &str, needle: &str) {
	assert!(body.contains(needle), "expected {needle:?} in:\n{body}");
}

#[track_caller]
pub fn assert_not_contains(body: &str, needle: &str) {
	assert!(
		!body.contains(needle),
		"did not expect {needle:?} in:\n{body}"
	);
}
