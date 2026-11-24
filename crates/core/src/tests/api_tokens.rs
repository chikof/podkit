//! The fixtures the API tokens tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `auth::tokens::tests`.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::tests::call;

pub fn tokens(team: i64) -> String {
	format!("/api/v1/teams/{team}/api-tokens")
}

/// Makes a token through the API as `session`; returns the whole response.
pub async fn make(app: &Router, session: &str, team: i64, extra: Value) -> Value {
	let mut body = json!({ "name": "ci", "scopes": ["read"] });
	body.as_object_mut()
		.unwrap()
		.extend(extra.as_object().unwrap().clone());
	let (s, created) = call(app, "POST", &tokens(team), Some(session), Some(body)).await;
	assert_eq!(s, StatusCode::OK, "create token: {created}");
	created
}

pub fn secret(created: &Value) -> &str {
	created["secret"].as_str().unwrap()
}
