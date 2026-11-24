//! Requests as a browser sends them: cookies, a `Host` and a same-origin `Origin`, GETs that
//! return the body as text, Datastar-style signal POSTs and body assertions. The page tests
//! of `podkit-ui` are built on these (its `testing` module adds the sign-in through the
//! pages); the tests of the cookie-authenticated routes of this crate use them too.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

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

/// A browser: the cookies it holds, as the server set them. A cookie the server removes
/// (an empty value, or `Max-Age=0`) leaves the jar.
#[derive(Default)]
pub struct Browser {
	pub jar: std::collections::BTreeMap<String, String>,
}

impl Browser {
	pub fn absorb(&mut self, headers: &HeaderMap) {
		for value in headers.get_all(header::SET_COOKIE) {
			let raw = value.to_str().unwrap();
			let first = raw.split(';').next().unwrap();
			let (name, val) = first.split_once('=').unwrap();
			let gone = val.is_empty() || raw.to_ascii_lowercase().contains("max-age=0");
			if gone {
				self.jar.remove(name);
			} else {
				self.jar.insert(name.to_owned(), val.to_owned());
			}
		}
	}

	pub fn cookie(&self) -> Option<String> {
		(!self.jar.is_empty()).then(|| {
			self.jar
				.iter()
				.map(|(k, v)| format!("{k}={v}"))
				.collect::<Vec<_>>()
				.join("; ")
		})
	}

	pub async fn get(&mut self, app: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
		let cookie = self.cookie();
		let out = get_html(app, uri, cookie.as_deref()).await;
		self.absorb(&out.1);
		out
	}

	pub async fn post(
		&mut self,
		app: &Router,
		uri: &str,
		signals: &Value,
	) -> (StatusCode, HeaderMap, String) {
		let cookie = self.cookie();
		let out = post_signals(app, uri, cookie.as_deref(), signals).await;
		self.absorb(&out.1);
		out
	}

	/// Whether the browser holds a session cookie.
	pub fn has_session(&self) -> bool {
		self.jar.contains_key("session")
	}
}
