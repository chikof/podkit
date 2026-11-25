//! Vendored static assets, served from the binary: fonts, the icon and logo sprites and
//! the one small script. Nothing is fetched from another origin, so podkit works offline.
//!
//! Every URL either carries a content hash (`?v=...`) or names a version, and is served
//! `immutable`, so browsers cache each forever and fetch again only when the bytes change.

use std::sync::LazyLock;

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use podkit_core::state::AppState;

pub static ICONS_SVG: &str = include_str!("../static/icons.svg");
pub static LOGOS_SVG: &str = include_str!("../static/logos.svg");
static PODKIT_JS: &str = include_str!("../static/podkit.js");
static PAGE_JS: &str = include_str!("../static/page.js");
static CANVAS_JS: &str = include_str!("../static/canvas.js");
static CHARTS_JS: &str = include_str!("../static/charts.js");
static GUARD_JS: &str = include_str!("../static/guard.js");
static TERMINAL_JS: &str = include_str!("../static/terminal.js");
// xterm.js 6.0.0 and its fit add-on 0.11.0 (MIT, see LICENSE-xterm.txt), as published on npm.
static XTERM_JS: &str = include_str!("../static/xterm.js");
static XTERM_FIT_JS: &str = include_str!("../static/xterm-fit.js");
static XTERM_CSS: &str = include_str!("../static/xterm.css");
static PASSKEY_JS: &str = include_str!("../static/passkey.js");
// Inter, JetBrains Mono and Sora, the latin subsets of the variable fonts as
// published by Fontsource 5.3.0 (OFL, see LICENSE-fonts-OFL.txt).
static INTER: &[u8] = include_bytes!("../static/fonts/inter-latin-wght-normal.woff2");
static JETBRAINS_MONO: &[u8] =
	include_bytes!("../static/fonts/jetbrains-mono-latin-wght-normal.woff2");
static SORA: &[u8] = include_bytes!("../static/fonts/sora-latin-wght-normal.woff2");
// The podoes peeker on a dark tile, from the podoes design system's Favicons group.
static FAVICON_SVG: &str = include_str!("../static/favicon.svg");
static FAVICON_PNG: &[u8] = include_bytes!("../static/favicon-32.png");
static APPLE_TOUCH_ICON: &[u8] = include_bytes!("../static/apple-touch-icon.png");

/// The `?v=` of the font URLs below and in `theme.css`'s `@font-face`; bump with the files.
pub static FONT_SANS_HREF: &str = "/static/fonts/inter-latin-wght-normal.woff2?v=1";
pub static FONT_MONO_HREF: &str = "/static/fonts/jetbrains-mono-latin-wght-normal.woff2?v=1";
pub static FONT_DISPLAY_HREF: &str = "/static/fonts/sora-latin-wght-normal.woff2?v=1";

fn hashed(path: &str, body: &str) -> String {
	hashed_bytes(path, body.as_bytes())
}

fn hashed_bytes(path: &str, body: &[u8]) -> String {
	let hash = Sha256::digest(body);
	format!("{path}?v={}", URL_SAFE_NO_PAD.encode(&hash[..6]))
}

/// The tab icon: the SVG, a 32 px PNG for a browser that takes no SVG icon, and the 180 px
/// square iOS puts on a home screen.
pub static FAVICON_SVG_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/favicon.svg", FAVICON_SVG));
pub static FAVICON_PNG_HREF: LazyLock<String> =
	LazyLock::new(|| hashed_bytes("/static/favicon-32.png", FAVICON_PNG));
pub static APPLE_TOUCH_ICON_HREF: LazyLock<String> =
	LazyLock::new(|| hashed_bytes("/static/apple-touch-icon.png", APPLE_TOUCH_ICON));

pub static ICONS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/icons.svg", ICONS_SVG));
pub static LOGOS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/logos.svg", LOGOS_SVG));
pub static PODKIT_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/podkit.js", PODKIT_JS));
pub static PAGE_JS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/page.js", PAGE_JS));
/// The canvas page's own script; no other page loads it.
pub static CANVAS_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/canvas.js", CANVAS_JS));
/// Loaded only by pages that draw a chart (see [`chart_script`]).
pub static CHARTS_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/charts.js", CHARTS_JS));
/// Loaded only by pages with a dialog or a save bar (see [`guard_script`]).
pub static GUARD_JS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/guard.js", GUARD_JS));
/// Loaded only by the pages that use passkeys (see [`passkey_script`]).
pub static PASSKEY_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/passkey.js", PASSKEY_JS));

/// The tag a page with a passkey button emits; once per page, and no other page loads it.
pub fn passkey_script() -> maud::Markup {
	maud::html! { script type="module" src=(*PASSKEY_JS_HREF) {} }
}

/// The web terminal's script and the vendored xterm.js it loads on demand. Only a page with a
/// terminal asks for them (see [`terminal_assets`]).
pub static TERMINAL_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/terminal.js", TERMINAL_JS));
pub static XTERM_JS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/xterm.js", XTERM_JS));
pub static XTERM_FIT_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/xterm-fit.js", XTERM_FIT_JS));
pub static XTERM_CSS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/xterm.css", XTERM_CSS));

/// The stylesheet and script of a terminal; once per page, and no other page loads them.
pub fn terminal_assets() -> maud::Markup {
	maud::html! {
		link rel="stylesheet" href=(*XTERM_CSS_HREF);
		script type="module" src=(*TERMINAL_JS_HREF) {}
	}
}

/// The tag a chart emits for its hover layer. A module loads once per page, however many
/// charts ask, and a page without one never downloads it.
pub fn chart_script() -> maud::Markup {
	maud::html! { script type="module" src=(*CHARTS_JS_HREF) {} }
}

/// The tag a dialog or save bar emits for the "Discard changes?" guard; once per page.
pub fn guard_script() -> maud::Markup {
	maud::html! { script type="module" src=(*GUARD_JS_HREF) {} }
}

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/static/icons.svg", get(icons))
		.route("/static/logos.svg", get(logos))
		.route("/static/podkit.js", get(podkit_js))
		.route("/static/page.js", get(page_js))
		.route("/static/canvas.js", get(canvas_js))
		.route("/static/charts.js", get(charts_js))
		.route("/static/guard.js", get(guard_js))
		.route("/static/terminal.js", get(terminal_js))
		.route("/static/xterm.js", get(xterm_js))
		.route("/static/xterm-fit.js", get(xterm_fit_js))
		.route("/static/xterm.css", get(xterm_css))
		.route("/static/passkey.js", get(passkey_js))
		.route("/static/favicon.svg", get(favicon_svg))
		.route("/static/favicon-32.png", get(favicon_png))
		.route("/static/apple-touch-icon.png", get(apple_touch_icon))
		.route("/static/fonts/inter-latin-wght-normal.woff2", get(sans))
		.route(
			"/static/fonts/jetbrains-mono-latin-wght-normal.woff2",
			get(mono),
		)
		.route("/static/fonts/sora-latin-wght-normal.woff2", get(display))
}

async fn icons() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "image/svg+xml"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		ICONS_SVG,
	)
}

async fn logos() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "image/svg+xml"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		LOGOS_SVG,
	)
}

async fn podkit_js() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		PODKIT_JS,
	)
}

async fn page_js() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		PAGE_JS,
	)
}

async fn canvas_js() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		CANVAS_JS,
	)
}

async fn charts_js() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		CHARTS_JS,
	)
}

async fn guard_js() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		GUARD_JS,
	)
}

async fn terminal_js() -> impl IntoResponse {
	script(TERMINAL_JS)
}

async fn xterm_js() -> impl IntoResponse {
	script(XTERM_JS)
}

async fn xterm_fit_js() -> impl IntoResponse {
	script(XTERM_FIT_JS)
}

async fn xterm_css() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/css; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		XTERM_CSS,
	)
}

async fn passkey_js() -> impl IntoResponse {
	script(PASSKEY_JS)
}

fn script(body: &'static str) -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		body,
	)
}

async fn favicon_svg() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "image/svg+xml"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		FAVICON_SVG,
	)
}

fn png(body: &'static [u8]) -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "image/png"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		body,
	)
}

async fn favicon_png() -> impl IntoResponse {
	png(FAVICON_PNG)
}

async fn apple_touch_icon() -> impl IntoResponse {
	png(APPLE_TOUCH_ICON)
}

fn font(body: &'static [u8]) -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "font/woff2"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		body,
	)
}

async fn sans() -> impl IntoResponse {
	font(INTER)
}

async fn mono() -> impl IntoResponse {
	font(JETBRAINS_MONO)
}

async fn display() -> impl IntoResponse {
	font(SORA)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_stylesheet_has_no_moz_focusring_preflight() {
		// Firefox-only, and it came after every focus style of ours (see build.rs).
		assert!(!crate::APP_CSS.contains(":-moz-focusring"));
		assert!(crate::APP_CSS.contains(":focus-visible{outline:2px solid var(--ring)"));
	}

	#[test]
	fn page_js_stays_under_its_budget_and_the_rest_loads_on_demand() {
		assert!(
			PAGE_JS.len() < 10 * 1024,
			"page.js is {} bytes",
			PAGE_JS.len()
		);
		// The chart and guard code live in their own files, not in page.js.
		assert!(!PAGE_JS.contains("data-points"));
		assert!(!PAGE_JS.contains("discard-confirm"));
		assert!(CHARTS_JS.contains("data-points") && GUARD_JS.contains("discard-confirm"));
	}

	#[test]
	fn hashed_urls_change_with_the_content() {
		assert_ne!(hashed("/a", "one"), hashed("/a", "two"));
		assert_eq!(hashed("/a", "one"), hashed("/a", "one"));
		assert!(ICONS_HREF.starts_with("/static/icons.svg?v="));
	}

	#[test]
	fn the_sprites_are_well_formed_and_every_symbol_has_an_id() {
		for sprite in [ICONS_SVG, LOGOS_SVG] {
			assert_eq!(
				sprite.matches("<symbol ").count(),
				sprite.matches("</symbol>").count()
			);
			assert!(sprite.matches("<symbol id=\"").count() > 20);
		}
	}
}
