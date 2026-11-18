//! The light/dark/system choice. It lives in the `theme` cookie and is written by the
//! server as a class on `<html>` (`dark` or `light`; none means follow the system), so
//! the first paint already has the right colors and nothing flashes on load.
//!
//! The toggle is a plain form posting to `/theme`: it works without JavaScript and the
//! page reloads with the new class. A middleware reads the cookie into a task-local, so
//! `layout::document` can render the class without every handler passing it along.

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use tower_cookies::cookie::SameSite;
use tower_cookies::cookie::time::Duration;
use tower_cookies::{Cookie, Cookies};

use crate::state::AppState;

const COOKIE: &str = "theme";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
	/// Follow the system setting.
	#[default]
	System,
	Light,
	Dark,
}

impl Theme {
	pub fn parse(value: &str) -> Self {
		match value {
			"light" => Self::Light,
			"dark" => Self::Dark,
			_ => Self::System,
		}
	}

	pub const fn as_str(self) -> &'static str {
		match self {
			Self::System => "system",
			Self::Light => "light",
			Self::Dark => "dark",
		}
	}

	/// The class for `<html>`; the system choice has none.
	pub const fn html_class(self) -> Option<&'static str> {
		match self {
			Self::System => None,
			Self::Light => Some("light"),
			Self::Dark => Some("dark"),
		}
	}

	/// The `<meta name="color-scheme">` value, so native widgets and the canvas match
	/// before the stylesheet arrives.
	pub const fn color_scheme(self) -> &'static str {
		match self {
			Self::System => "dark light",
			Self::Light => "light",
			Self::Dark => "dark",
		}
	}
}

tokio::task_local! {
	static CURRENT: Theme;
}

/// The theme of the request being handled; the system choice outside a request.
pub fn current() -> Theme {
	CURRENT.try_with(|t| *t).unwrap_or_default()
}

fn from_cookie_header(headers: &HeaderMap) -> Theme {
	headers
		.get_all(header::COOKIE)
		.iter()
		.filter_map(|v| v.to_str().ok())
		.flat_map(|v| v.split(';'))
		.filter_map(|pair| pair.trim().strip_prefix("theme="))
		.map(Theme::parse)
		.next()
		.unwrap_or_default()
}

/// Makes the request's theme available to `current()` while the handler runs.
pub async fn scope(req: Request, next: Next) -> Response {
	let theme = from_cookie_header(req.headers());
	CURRENT.scope(theme, next.run(req)).await
}

pub fn router() -> Router<AppState> {
	Router::new().route("/theme", post(set))
}

/// The page the form was on, as a local path; `/` when unknown or not ours.
fn back(headers: &HeaderMap) -> String {
	headers
		.get(header::REFERER)
		.and_then(|v| v.to_str().ok())
		.and_then(|r| url::Url::parse(r).ok())
		.map(|u| match u.query() {
			Some(q) => format!("{}?{q}", u.path()),
			None => u.path().to_owned(),
		})
		.filter(|p| super::safe_next(p) == p)
		.unwrap_or_else(|| "/".to_owned())
}

/// `POST /theme` with `theme=light|dark|system` (form-encoded). Needs no session: the
/// choice is cosmetic. Anything else is read as `system`.
async fn set(cookies: Cookies, headers: HeaderMap, body: String) -> Response {
	if body.len() > 256 {
		return StatusCode::PAYLOAD_TOO_LARGE.into_response();
	}
	let choice = url::form_urlencoded::parse(body.as_bytes())
		.find(|(k, _)| k == COOKIE)
		.map_or(Theme::System, |(_, v)| Theme::parse(&v));
	match choice {
		// No cookie means "follow the system"; remove it where it was set.
		Theme::System => cookies.remove(Cookie::build((COOKIE, "")).path("/").build()),
		other => cookies.add(
			Cookie::build((COOKIE, other.as_str()))
				.path("/")
				.same_site(SameSite::Lax)
				.max_age(Duration::days(365))
				.build(),
		),
	}
	Redirect::to(&back(&headers)).into_response()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cookie_header(value: &str) -> HeaderMap {
		let mut h = HeaderMap::new();
		h.insert(header::COOKIE, value.parse().unwrap());
		h
	}

	#[test]
	fn the_cookie_picks_the_theme_and_anything_else_is_system() {
		assert_eq!(
			from_cookie_header(&cookie_header("theme=dark")),
			Theme::Dark
		);
		assert_eq!(
			from_cookie_header(&cookie_header("session=x; theme=light; a=b")),
			Theme::Light
		);
		assert_eq!(
			from_cookie_header(&cookie_header("theme=pink")),
			Theme::System
		);
		assert_eq!(
			from_cookie_header(&cookie_header("mytheme=dark")),
			Theme::System
		);
		assert_eq!(from_cookie_header(&HeaderMap::new()), Theme::System);
	}

	#[test]
	fn only_an_explicit_choice_writes_a_class() {
		assert_eq!(Theme::System.html_class(), None);
		assert_eq!(Theme::Light.html_class(), Some("light"));
		assert_eq!(Theme::Dark.html_class(), Some("dark"));
		assert_eq!(Theme::System.color_scheme(), "dark light");
	}

	#[test]
	fn the_form_returns_to_a_local_page_only() {
		let referer = |r: &str| {
			let mut h = HeaderMap::new();
			h.insert(header::REFERER, r.parse().unwrap());
			back(&h)
		};
		assert_eq!(
			referer("http://podkit.test/teams/1/projects?x=1"),
			"/teams/1/projects?x=1"
		);
		assert_eq!(referer("http://podkit.test//evil.example"), "/");
		assert_eq!(back(&HeaderMap::new()), "/");
	}

	#[tokio::test]
	async fn current_is_the_request_theme_inside_a_scope_and_system_outside() {
		assert_eq!(current(), Theme::System);
		let inside = CURRENT.scope(Theme::Dark, async { current() }).await;
		assert_eq!(inside, Theme::Dark);
	}
}
