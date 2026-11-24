//! The notification bell in the header: a link to the team's notification history with the
//! number of events the signed-in person has not read yet. A link, not a menu: it works
//! without script and the history is a page of its own.

use maud::{Markup, html};

use super::icon::icon;

/// Most the badge counts to; more reads "99+".
const MAX_SHOWN: i64 = 99;

/// The accessible name: it carries the count, because the badge itself is decoration.
pub fn bell_label(unread: i64) -> String {
	match unread {
		..=0 => "Notifications".to_owned(),
		1 => "Notifications, 1 unread".to_owned(),
		n => format!("Notifications, {n} unread"),
	}
}

pub fn bell(team: i64, unread: i64) -> Markup {
	let shown = if unread > MAX_SHOWN {
		format!("{MAX_SHOWN}+")
	} else {
		unread.to_string()
	};
	html! {
		a href={ "/teams/" (team) "/notifications" } class="ui-icon-btn ui-bell" aria-label=(bell_label(unread)) {
			(icon("bell"))
			@if unread > 0 {
				span class="ui-bell-count" aria-hidden="true" { (shown) }
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_badge_shows_the_count_and_caps_it() {
		let none = bell(1, 0).into_string();
		assert!(!none.contains("ui-bell-count"), "{none}");
		assert!(none.contains("aria-label=\"Notifications\""), "{none}");
		let some = bell(1, 3).into_string();
		assert!(some.contains(">3<"), "{some}");
		assert!(some.contains("Notifications, 3 unread"), "{some}");
		assert!(bell(1, 250).into_string().contains("99+"));
	}
}
