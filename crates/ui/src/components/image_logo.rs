//! `image_logo(url, size, priority)`: a template's own logo (an SVG or PNG served by
//! `/static/logos/…`) in the same rounded tile as `logo`, but on a light plate in both color
//! schemes, because registry marks are drawn for a light page and some are black.
//!
//! The image is decorative: the name next to it carries the meaning, so `alt` is empty and
//! the tile is `aria-hidden`. Size attributes are always given so the page never shifts
//! while the image loads. Logos below the fold lazy-load and fetch at low priority.

use maud::{Markup, html};

use super::logo::LogoSize;

const SM: &str = "ui-logo ui-logo-sm ui-logo-img";
const MD: &str = "ui-logo ui-logo-md ui-logo-img";
const LG: &str = "ui-logo ui-logo-lg ui-logo-img";

/// Whether the image is likely in the first screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
	/// Above the fold: load normally.
	Normal,
	/// Below the fold: lazy, low priority.
	Low,
}

const fn class(size: LogoSize) -> &'static str {
	match size {
		LogoSize::Sm => SM,
		LogoSize::Md => MD,
		LogoSize::Lg => LG,
	}
}

/// Pixel size the tile has at the default root font size, for the `width`/`height`
/// attributes (the CSS decides the real size).
const fn pixels(size: LogoSize) -> u16 {
	match size {
		LogoSize::Sm => 28,
		LogoSize::Md => 40,
		LogoSize::Lg => 56,
	}
}

pub fn image_logo(url: &str, size: LogoSize, priority: Priority) -> Markup {
	let px = pixels(size);
	html! {
		span class=(class(size)) aria-hidden="true" data-logo="image" {
			@if priority == Priority::Low {
				img src=(url) alt="" width=(px) height=(px) loading="lazy" decoding="async" fetchpriority="low";
			} @else {
				img src=(url) alt="" width=(px) height=(px) decoding="async";
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_image_is_decorative_sized_and_escaped() {
		let html =
			image_logo("/static/logos/a\"b.svg", LogoSize::Md, Priority::Normal).into_string();
		assert!(html.contains("alt=\"\""), "{html}");
		assert!(html.contains("width=\"40\" height=\"40\""), "{html}");
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(html.contains("a&quot;b.svg"), "{html}");
		assert!(!html.contains("loading="), "{html}");
	}

	#[test]
	fn a_logo_below_the_fold_lazy_loads_at_low_priority() {
		let html = image_logo("/x.svg", LogoSize::Sm, Priority::Low).into_string();
		assert!(html.contains("loading=\"lazy\""), "{html}");
		assert!(html.contains("fetchpriority=\"low\""), "{html}");
	}
}
