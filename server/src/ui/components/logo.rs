//! `logo(name, size)`: a service's mark in a rounded tile, or a monogram when podkit has
//! no mark for it. Marks are single-color Simple Icons (CC0) in `/static/logos.svg`,
//! drawn in the text color so they read in both themes. The name next to the logo
//! carries the meaning, so the tile is `aria-hidden`.

use maud::{Markup, html};

use crate::ui::assets::{LOGOS_HREF, LOGOS_SVG};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSize {
	Sm,
	Md,
	Lg,
}

const SM: &str = "ui-logo ui-logo-sm";
const MD: &str = "ui-logo ui-logo-md";
const LG: &str = "ui-logo ui-logo-lg";

impl LogoSize {
	const fn class(self) -> &'static str {
		match self {
			Self::Sm => SM,
			Self::Md => MD,
			Self::Lg => LG,
		}
	}
}

/// The sprite id for a service name as people write it (`Postgres`, `node`, `Next.js`),
/// or `None` when there is no mark for it.
fn mark_for(name: &str) -> Option<String> {
	let key: String = name
		.trim()
		.to_ascii_lowercase()
		.chars()
		.filter(char::is_ascii_alphanumeric)
		.collect();
	let slug = match key.as_str() {
		"postgres" | "pg" => "postgresql",
		"mongo" => "mongodb",
		"node" | "nodejs" => "nodedotjs",
		"next" | "nextjs" => "nextdotjs",
		"vue" | "vuejs" => "vuedotjs",
		"golang" => "go",
		"java" => "openjdk",
		"kafka" => "apachekafka",
		"hass" => "homeassistant",
		"paperless" => "paperlessngx",
		"plausible" => "plausibleanalytics",
		other => other,
	};
	LOGOS_SVG
		.contains(&format!("<symbol id=\"{slug}\""))
		.then(|| slug.to_owned())
}

/// The letter shown when there is no mark: the first letter or digit, upper-cased.
fn monogram(name: &str) -> String {
	name.chars()
		.find(|c| c.is_alphanumeric())
		.map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

pub fn logo(name: &str, size: LogoSize) -> Markup {
	let Some(slug) = mark_for(name) else {
		return html! {
			span class=(size.class()) aria-hidden="true" data-logo="monogram" { (monogram(name)) }
		};
	};
	html! {
		span class=(size.class()) aria-hidden="true" data-logo=(slug) {
			svg focusable="false" {
				use href={ (*LOGOS_HREF) "#" (slug) } {}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_known_service_gets_its_mark_by_name_or_alias() {
		for name in ["postgres", "PostgreSQL", " Postgres ", "pg"] {
			let html = logo(name, LogoSize::Md).into_string();
			assert!(html.contains("data-logo=\"postgresql\""), "{name}: {html}");
			assert!(html.contains("/static/logos.svg?v="), "{html}");
		}
		assert!(
			logo("Next.js", LogoSize::Sm)
				.into_string()
				.contains("#nextdotjs")
		);
	}

	#[test]
	fn an_unknown_service_falls_back_to_a_monogram() {
		let html = logo("zebra-db", LogoSize::Lg).into_string();
		assert!(html.contains("data-logo=\"monogram\""), "{html}");
		assert!(html.contains(">Z<"), "{html}");
		assert!(!html.contains("<svg"), "{html}");
		assert!(logo("", LogoSize::Sm).into_string().contains(">?<"));
		assert!(logo("--x", LogoSize::Sm).into_string().contains(">X<"));
	}

	#[test]
	fn the_tile_is_decorative_and_the_name_never_becomes_markup() {
		let html = logo("<script>alert(1)</script>", LogoSize::Md).into_string();
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(!html.contains("<script>"), "{html}");
		let html = logo("\"><img src=x>", LogoSize::Md).into_string();
		assert!(!html.contains("<img"), "{html}");
	}

	#[test]
	fn the_sizes_have_distinct_classes() {
		assert!(logo("x", LogoSize::Sm).into_string().contains("ui-logo-sm"));
		assert!(logo("x", LogoSize::Lg).into_string().contains("ui-logo-lg"));
	}
}
