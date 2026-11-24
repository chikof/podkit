//! API tokens: scope, team and project limits, expiry, revocation, and the secret's
//! handling. `cargo test -p podkit-ui --all-features -- api_tokens`
//!
//! The tests of `podkit_core::auth::tokens` that need the pages: they sign in through the
//! web UI or read what it renders, so they run against the whole router. The rest are beside
//! the code, in `podkit_core::auth::tokens::tests`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;

use crate::tests::{app, call, user_with_team};
use podkit_core::tests::api_tokens::{make, secret};

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn api_tokens_cannot_use_the_ui_or_the_auth_routes(db: PgPool) {
	let app = app(db).await;
	let (session, team) = user_with_team(&app, "alice").await;
	let token = make(&app, &session, team, json!({ "scopes": ["root"] })).await;
	let t = secret(&token);

	for path in [
		"/".to_owned(),
		format!("/teams/{team}/projects"),
		format!("/teams/{team}/settings"),
		format!("/teams/{team}/settings/api-tokens"),
	] {
		let resp = app
			.clone()
			.oneshot(
				Request::builder()
					.uri(&path)
					.header(header::AUTHORIZATION, format!("Bearer {t}"))
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();
		assert_ne!(resp.status(), StatusCode::OK, "{path} answered a token");
	}
	let (s, _) = call(&app, "POST", "/api/v1/auth/logout", Some(t), None).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	// The token is not a session, so the UI-only cookie path cannot take it either.
	let resp = app
		.clone()
		.oneshot(
			Request::builder()
				.uri(format!("/teams/{team}/projects"))
				.header(header::COOKIE, format!("session={t}"))
				.body(Body::empty())
				.unwrap(),
		)
		.await
		.unwrap();
	assert_ne!(resp.status(), StatusCode::OK);
}
