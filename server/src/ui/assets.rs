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

use crate::state::AppState;

pub static ICONS_SVG: &str = include_str!("../../static/icons.svg");
pub static LOGOS_SVG: &str = include_str!("../../static/logos.svg");
static PODKIT_JS: &str = include_str!("../../static/podkit.js");
static PAGE_JS: &str = include_str!("../../static/page.js");
static HANKEN_GROTESK: &[u8] = include_bytes!("../../static/fonts/hanken-grotesk-var.woff2");
static SPLINE_SANS_MONO: &[u8] = include_bytes!("../../static/fonts/spline-sans-mono-var.woff2");

/// The `?v=` of the font URLs below and in `theme.css`'s `@font-face`; bump with the files.
pub static FONT_SANS_HREF: &str = "/static/fonts/hanken-grotesk-var.woff2?v=1";
pub static FONT_MONO_HREF: &str = "/static/fonts/spline-sans-mono-var.woff2?v=1";

fn hashed(path: &str, body: &str) -> String {
	let hash = Sha256::digest(body.as_bytes());
	format!("{path}?v={}", URL_SAFE_NO_PAD.encode(&hash[..6]))
}

pub static ICONS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/icons.svg", ICONS_SVG));
pub static LOGOS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/logos.svg", LOGOS_SVG));
pub static PODKIT_JS_HREF: LazyLock<String> =
	LazyLock::new(|| hashed("/static/podkit.js", PODKIT_JS));
pub static PAGE_JS_HREF: LazyLock<String> = LazyLock::new(|| hashed("/static/page.js", PAGE_JS));

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/static/icons.svg", get(icons))
		.route("/static/logos.svg", get(logos))
		.route("/static/podkit.js", get(podkit_js))
		.route("/static/page.js", get(page_js))
		.route("/static/fonts/hanken-grotesk-var.woff2", get(sans))
		.route("/static/fonts/spline-sans-mono-var.woff2", get(mono))
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

async fn sans() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "font/woff2"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		HANKEN_GROTESK,
	)
}

async fn mono() -> impl IntoResponse {
	(
		[
			(header::CONTENT_TYPE, "font/woff2"),
			(header::CACHE_CONTROL, IMMUTABLE),
		],
		SPLINE_SANS_MONO,
	)
}

#[cfg(test)]
mod tests {
	use super::*;

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
