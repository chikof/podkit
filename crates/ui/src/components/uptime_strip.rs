//! The day strip of an uptime check: one small bar per UTC day, oldest first, coloured by how
//! much of the day the check was up. No script. The strip is one image to assistive
//! technology: its label carries the percentage, so the colours are never the only message.

use maud::{Markup, html};
use time::{Date, Duration};

use podkit_core::uptime::DayStat;

/// How a day reads: no runs, up the whole day (99.5% or more), partly down, or mostly down.
fn level(stat: Option<&DayStat>) -> &'static str {
	match stat.and_then(|s| s.percent()) {
		None => "none",
		Some(p) if p >= 99.5 => "ok",
		Some(p) if p >= 90.0 => "partial",
		Some(_) => "down",
	}
}

/// `99.95%`, with as many decimals as it takes to not round a bad day up to 100.
#[must_use]
pub fn percent_text(p: f64) -> String {
	let text = if p >= 100.0 {
		return "100%".to_owned();
	} else if p >= 99.0 {
		// Truncate: 99.996 must not read as 100.
		format!("{:.2}", (p * 100.0).floor() / 100.0)
	} else {
		format!("{:.1}", (p * 10.0).floor() / 10.0)
	};
	format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
}

/// `span` days ending `today`, each from `days` when it has an entry. `label` is what the strip
/// says as a whole (`Last 90 days: 99.95% up`).
#[must_use]
pub fn uptime_strip(days: &[DayStat], span: i64, today: Date, label: &str) -> Markup {
	html! {
		ol class="ui-strip" role="img" aria-label=(label) {
			@for back in (0..span).rev() {
				@let day = today - Duration::days(back);
				@let stat = days.iter().find(|d| d.day == day);
				li class="ui-strip-day" data-level=(level(stat)) title=(match stat.and_then(|s| s.percent()) {
					Some(p) => format!("{day}: {}", percent_text(p)),
					None => format!("{day}: no data"),
				}) {}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use time::macros::date;

	use super::*;

	#[test]
	fn a_bad_day_is_never_rounded_up_to_perfect() {
		assert_eq!(percent_text(100.0), "100%");
		assert_eq!(percent_text(99.996), "99.99%");
		assert_eq!(percent_text(97.34), "97.3%");
	}

	#[test]
	fn the_strip_has_one_cell_per_day_and_says_its_total() {
		let today = date!(2026 - 11 - 20);
		let days = [
			DayStat {
				day: today,
				total: 10,
				ok: 10,
			},
			DayStat {
				day: today - Duration::days(1),
				total: 10,
				ok: 2,
			},
		];
		let html = uptime_strip(&days, 5, today, "Last 5 days: 60% up").into_string();
		assert_eq!(html.matches("ui-strip-day").count(), 5, "{html}");
		assert!(
			html.contains("aria-label=\"Last 5 days: 60% up\""),
			"{html}"
		);
		assert!(html.contains("data-level=\"down\""), "{html}");
		assert!(html.contains("data-level=\"none\""), "{html}");
	}
}
