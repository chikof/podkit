//! The tests of the crate root: sign-in and sign-out through the pages, the origin guard,
//! the static files and the invariants every template keeps.

use crate::tests::{PgPool, db_test};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use crate::testing::{
	assert_contains, assert_not_contains, get_html, post_signals, send, ui_session,
};
use crate::tests::{app, count, user_with_team};
use crate::{safe_next, same_origin};

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn cookie_session_signs_in_lands_on_its_team_and_signs_out(db: PgPool) {
	let app = app(db).await;
	let (_, team) = user_with_team(&app, "alice").await;

	let (s, h, _) = get_html(&app, "/", None).await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert_eq!(h[header::LOCATION], "/login");

	let wrong = r#"{"email":"alice@example.com","password":"nope"}"#;
	let (s, h, body) = send(&app, "POST", "/login", None, Some(wrong), None).await;
	assert_eq!(s, StatusCode::OK);
	assert!(h.get(header::SET_COOKIE).is_none());
	assert_contains(&body, "Wrong email or password.");

	let right = r#"{"email":"alice@example.com","password":"correct horse battery"}"#;
	let (s, h, body) = send(&app, "POST", "/login", None, Some(right), None).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&body, "window.location.assign('/')");
	let set_cookie = h[header::SET_COOKIE].to_str().unwrap();
	assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
	assert!(
		!set_cookie.contains("Secure"),
		"plain HTTP must get a usable cookie: {set_cookie}"
	);
	let cookie = set_cookie.split(';').next().unwrap().to_owned();

	let (s, h, _) = get_html(&app, "/", Some(&cookie)).await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert_eq!(
		h[header::LOCATION],
		format!("/teams/{team}/projects").as_str()
	);

	let (s, h, _) = send(&app, "POST", "/logout", Some(&cookie), None, None).await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert_eq!(h[header::LOCATION], "/login");

	let (s, _, _) = get_html(&app, "/", Some(&cookie)).await;
	assert_eq!(
		s,
		StatusCode::SEE_OTHER,
		"revoked session must not render the page"
	);
	let (s, h, _) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert!(h[header::LOCATION].to_str().unwrap().starts_with("/login"));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn session_cookie_is_secure_behind_an_https_proxy(db: PgPool) {
	let app = app(db).await;
	user_with_team(&app, "alice").await;

	let req = Request::post("/login")
		.header(header::CONTENT_TYPE, "application/json")
		.header("datastar-request", "true")
		.header("x-forwarded-proto", "https")
		.body(Body::from(
			r#"{"email":"alice@example.com","password":"correct horse battery"}"#,
		))
		.unwrap();
	let resp = app.oneshot(req).await.unwrap();
	let set_cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
	assert!(set_cookie.contains("Secure"), "{set_cookie}");
}

/// The browser lower-cases attribute names, so a `data-bind` key with capitals reaches Datastar as the
/// signal `newname` and never matches `$newName` or the `newName` the server reads. A
/// camelCase signal is bound with the value form instead: `data-bind="newName"`.
#[test]
fn no_template_names_a_signal_in_the_attribute_key_with_capitals() {
	fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
		for entry in std::fs::read_dir(dir).unwrap() {
			let path = entry.unwrap().path();
			if path.is_dir() {
				walk(&path, found);
			} else if path.extension().is_some_and(|e| e == "rs") {
				let src = std::fs::read_to_string(&path).unwrap();
				for (n, line) in src.lines().enumerate() {
					for kind in ["bind", "signals", "computed", "ref", "indicator"] {
						let needle = format!("data-{kind}:");
						let mut rest = line;
						while let Some(at) = rest.find(&needle) {
							let after = &rest[at + needle.len()..];
							let key: String = after
								.chars()
								.take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
								.collect();
							if key.chars().any(|c| c.is_ascii_uppercase()) {
								found.push(format!("{}:{}: {line}", path.display(), n + 1));
							}
							rest = after;
						}
					}
				}
			}
		}
	}
	let mut found = Vec::new();
	walk(
		&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
		&mut found,
	);
	assert!(
		found.is_empty(),
		"capitals in an attribute key:\n{}",
		found.join("\n")
	);
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn the_sign_in_and_sign_up_pages_do_not_overflow_the_viewport(db: PgPool) {
	// A top margin on the page's `main` collapses through the full-height body and makes the
	// page scroll by that much; the space has to be padding.
	let app = app(db).await;
	for path in ["/login", "/register"] {
		let (s, _, page) = get_html(&app, path, None).await;
		assert_eq!(s, StatusCode::OK, "{path}");
		assert_contains(&page, "class=\"ui-auth\"");
		assert!(
			!page.contains("mt-16"),
			"{path}: margin collapses out of the body"
		);
	}
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn unauthenticated_page_redirects_to_login_with_next_and_sign_in_returns(db: PgPool) {
	let app = app(db).await;
	user_with_team(&app, "alice").await;

	let (s, h, _) = get_html(&app, "/teams/1/projects", None).await;
	assert_eq!(s, StatusCode::SEE_OTHER);
	assert_eq!(h[header::LOCATION], "/login?next=%2Fteams%2F1%2Fprojects");

	let (s, _, page) = get_html(&app, "/login?next=%2Fteams%2F1%2Fprojects", None).await;
	assert_eq!(s, StatusCode::OK);
	assert_contains(&page, "next: '/teams/1/projects'");

	let creds = json!({
		"email": "alice@example.com", "password": "correct horse battery",
		"next": "/teams/1/projects",
	});
	let (_, _, body) = post_signals(&app, "/login", None, &creds).await;
	assert_contains(&body, "window.location.assign('/teams/1/projects')");
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn sign_in_ignores_a_next_that_leaves_the_site(db: PgPool) {
	let app = app(db).await;
	user_with_team(&app, "alice").await;

	for next in [
		"//evil.example",
		"https://evil.example",
		"/\\evil.example",
		"javascript:alert(1)",
	] {
		let creds = json!({
			"email": "alice@example.com", "password": "correct horse battery", "next": next,
		});
		let (_, _, body) = post_signals(&app, "/login", None, &creds).await;
		assert_contains(&body, "window.location.assign('/')");
		assert_not_contains(&body, "evil");

		let uri = format!(
			"/login?next={}",
			next.replace('/', "%2F").replace('\\', "%5C")
		);
		let (_, _, page) = get_html(&app, &uri, None).await;
		assert_contains(&page, "next: '/'");
		assert_not_contains(&page, "evil");
	}
}

#[test]
fn safe_next_accepts_only_local_paths() {
	assert_eq!(safe_next("/teams/1?a=b"), "/teams/1?a=b");
	for bad in ["", "//x", "x", "http://x", "/\\x", "/a\nb"] {
		assert_eq!(safe_next(bad), "/", "{bad:?}");
	}
}

#[test]
fn same_origin_compares_origin_with_host_or_forwarded_host() {
	let h = |pairs: &[(&str, &str)]| {
		let mut m = axum::http::HeaderMap::new();
		for (k, v) in pairs {
			m.insert(
				axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
				v.parse().unwrap(),
			);
		}
		m
	};
	assert!(same_origin(&h(&[
		("origin", "http://a.test:8080"),
		("host", "a.test:8080")
	])));
	assert!(!same_origin(&h(&[
		("origin", "http://evil.example"),
		("host", "a.test")
	])));
	assert!(!same_origin(&h(&[("origin", "null"), ("host", "a.test")])));
	assert!(!same_origin(&h(&[("origin", "http://a.test")])));
	assert!(same_origin(&h(&[
		("origin", "https://pk.example"),
		("host", "10.0.0.2:8080"),
		("x-forwarded-host", "pk.example"),
	])));
	assert!(same_origin(&h(&[("host", "a.test")])));
	assert!(!same_origin(&h(&[
		("host", "a.test"),
		("sec-fetch-site", "cross-site")
	])));
}

#[db_test(migrator = "podkit_core::db::MIGRATOR")]
async fn a_cross_origin_post_is_refused_and_changes_nothing(db: PgPool) {
	let app = app(db.clone()).await;
	let (cookie, team) = ui_session(&app, "alice").await;

	let evil = "http://evil.example";
	let body = json!({ "name": "Evil", "slug": "evil" }).to_string();
	for uri in [format!("/teams/{team}/projects"), "/teams/new".to_owned()] {
		let (s, _, _) = send(&app, "POST", &uri, Some(&cookie), Some(&body), Some(evil)).await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
	}
	let (s, _, _) = send(&app, "POST", "/logout", Some(&cookie), None, Some(evil)).await;
	assert_eq!(s, StatusCode::FORBIDDEN);

	assert_eq!(count(&db, "projects").await, 0);
	assert_eq!(count(&db, "teams").await, 1);
	let (s, _, _) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
	assert_eq!(s, StatusCode::OK, "the session survives a refused logout");
}
