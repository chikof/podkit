//! The whole router: what only shows once the web UI is mounted beside the API. The tests of
//! the API's own mounts are in `podkit_core::routes`.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::tests::{app, call, user_with_team};

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn text_assets_are_gzipped_and_json_and_fonts_are_not(db: PgPool) {
	use axum::body::Body;
	use axum::http::Request;
	use tower::ServiceExt;

	let app = app(db).await;
	let get = |uri: &'static str| {
		let app = app.clone();
		async move {
			app.oneshot(
				Request::builder()
					.uri(uri)
					.header("accept-encoding", "gzip")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap()
		}
	};
	let r = get("/static/app.css").await;
	assert_eq!(r.status(), StatusCode::OK);
	assert_eq!(r.headers()["content-encoding"], "gzip", "css");
	let r = get("/static/page.js").await;
	assert_eq!(r.headers()["content-encoding"], "gzip", "js");
	let r = get("/login").await;
	assert_eq!(r.headers()["content-encoding"], "gzip", "html");
	let r = get("/api/v1/catalog").await;
	assert!(
		r.headers().get("content-encoding").is_none(),
		"json stays plain"
	);
	let r = get("/static/fonts/inter-latin-wght-normal.woff2").await;
	assert!(
		r.headers().get("content-encoding").is_none(),
		"fonts stay plain"
	);
}

#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
async fn old_api_paths_are_not_json(db: PgPool) {
	let app = app(db).await;
	let (token, team) = user_with_team(&app, "alice").await;
	for path in ["/teams".to_owned(), "/auth/login".to_owned()] {
		let (s, body) = call(&app, "GET", &path, Some(&token), None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
		assert!(body.is_null(), "{path} returned JSON: {body}");
	}
	// The web UI owns this path now: it answers with a page, never JSON.
	let path = format!("/teams/{team}/projects");
	let (_, body) = call(&app, "GET", &path, Some(&token), None).await;
	assert!(body.is_null(), "{path} returned JSON: {body}");
}
