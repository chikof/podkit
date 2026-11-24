//! Progress: a native `<progress>` (a screen reader gets the role and value for free)
//! with the figures it stands for under it, as text. Styled in `components.css`.

use maud::{Markup, html};

/// `label` names the bar, `percent` is 0 to 100, `figures` is the text under it
/// ("1.2 GB of 2.5 GB · 50%").
pub fn progress(label: &str, percent: u64, figures: &str) -> Markup {
	let percent = percent.min(100);
	html! {
		div class="flex flex-col gap-2" {
			progress class="ui-progress" max="100" value=(percent) aria-label=(label) {}
			span class="text-xs text-muted-foreground tabular-nums" { (figures) }
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_bar_is_native_named_and_capped_at_a_hundred() {
		let html = progress("Download of <repo>", 250, "1 GB of 1 GB").into_string();
		assert!(
			html.contains("<progress class=\"ui-progress\" max=\"100\" value=\"100\""),
			"{html}"
		);
		assert!(
			html.contains("aria-label=\"Download of &lt;repo&gt;\""),
			"{html}"
		);
		assert!(html.contains("1 GB of 1 GB"), "{html}");
	}
}
