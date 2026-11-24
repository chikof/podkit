//! Test helpers for the web UI: a signed-in cookie session, GETs that return
//! HTML, Datastar-style signal POSTs and body assertions.

use axum::Router;
use axum::http::{StatusCode, header};
pub use podkit_core::tests::browser::*;
use serde_json::json;

use crate::tests::user_with_team;

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
