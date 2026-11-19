//! What the application and stack pages share: the head (logo, eyebrow, name, status line,
//! actions), the row of live figures under it, and the status words of deployments, stacks
//! and services drawn as Quiet's shaped dots.
//!
//! A status is never colour alone: each key has its own dot shape in `components.css`, and
//! the word always sits next to it.

use maud::{Markup, html};

use crate::deployments::DeploymentStatus;
use crate::stacks::{ServiceStatus, StackStatus};
use crate::ui::components::Status as Shape;

/// A dot and a word. `shape` picks the colour and form; `word` is what the page says.
pub fn dot(shape: Shape, word: &str) -> Markup {
	html! {
		span class="ui-badge" data-status=(shape.key()) {
			span class="ui-dot" aria-hidden="true" {}
			span { (word) }
		}
	}
}

/// The shape of a deployment's state. Queued waits, cancelled and stopped are at rest, and
/// crashed is a failure.
#[must_use]
pub const fn deployment_shape(s: DeploymentStatus) -> Shape {
	match s {
		DeploymentStatus::Queued => Shape::Pending,
		DeploymentStatus::Building => Shape::Building,
		DeploymentStatus::Deploying => Shape::Deploying,
		DeploymentStatus::Running => Shape::Running,
		DeploymentStatus::Failed | DeploymentStatus::Crashed => Shape::Failed,
		DeploymentStatus::Stopped | DeploymentStatus::Cancelled => Shape::Stopped,
	}
}

/// The shape of a stack's state; a stack being removed reads as at rest.
#[must_use]
pub const fn stack_shape(s: StackStatus) -> Shape {
	match s {
		StackStatus::Pending => Shape::Pending,
		StackStatus::Deploying => Shape::Deploying,
		StackStatus::Running => Shape::Running,
		StackStatus::Stopped | StackStatus::Deleting => Shape::Stopped,
		StackStatus::Failed => Shape::Failed,
	}
}

/// The shape of one service's state.
#[must_use]
pub const fn service_shape(s: ServiceStatus) -> Shape {
	match s {
		ServiceStatus::Pending => Shape::Pending,
		ServiceStatus::Running => Shape::Running,
		ServiceStatus::Stopped | ServiceStatus::Removing => Shape::Stopped,
		ServiceStatus::Failed => Shape::Failed,
	}
}

/// The head of a resource page. `logo` is the resource's mark, `eyebrow` its kind ("Git
/// app"), `sub` the status line under the name, and `actions` the buttons on the right.
pub fn head(logo: &Markup, eyebrow: &Markup, name: &str, sub: &Markup, actions: &Markup) -> Markup {
	html! {
		div class="ui-head" {
			span class="ui-head-logo" { (logo) }
			div class="ui-head-id" {
				p class="ui-eyebrow" { (eyebrow) }
				h1 { (name) }
				div class="ui-head-sub" { (sub) }
			}
			(actions)
		}
	}
}

/// The row of figures under the head: a label and a value each. Values that are not known
/// are an em dash, so the row never changes shape.
pub fn stats(label: &str, items: &[(&str, String)]) -> Markup {
	html! {
		dl class="ui-stats" aria-label=(label) {
			@for (name, value) in items {
				div {
					dt { (name) }
					dd { (value) }
				}
			}
		}
	}
}

/// `412 MB`, `1.2 GB`: base-1000 like the rest of podkit's sizes.
#[must_use]
pub fn bytes(n: i64) -> String {
	const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
	let mut value = n.max(0) as f64;
	let mut unit = 0;
	while value >= 1000.0 && unit < UNITS.len() - 1 {
		value /= 1000.0;
		unit += 1;
	}
	if unit == 0 || value >= 100.0 {
		format!("{value:.0} {}", UNITS[unit])
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

/// `12 min`, `3 h`, `2 d`: the one largest unit, for a figure that has little room.
#[must_use]
pub fn span(d: time::Duration) -> String {
	let secs = d.whole_seconds().max(0);
	match secs {
		0..=59 => format!("{secs} s"),
		60..=3599 => format!("{} min", secs / 60),
		3600..=86_399 => format!("{} h", secs / 3600),
		_ => format!("{} d", secs / 86_400),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_state_has_a_shape_and_the_word_is_always_shown() {
		let html = dot(deployment_shape(DeploymentStatus::Crashed), "Crashed").into_string();
		assert!(html.contains("data-status=\"failed\""), "{html}");
		assert!(html.contains("<span>Crashed</span>"), "{html}");
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert_eq!(stack_shape(StackStatus::Deleting), Shape::Stopped);
		assert_eq!(service_shape(ServiceStatus::Removing), Shape::Stopped);
		assert_eq!(deployment_shape(DeploymentStatus::Queued), Shape::Pending);
	}

	#[test]
	fn sizes_and_spans_read_in_one_unit() {
		assert_eq!(bytes(412_000_000), "412 MB");
		assert_eq!(bytes(1_181_116_006), "1.2 GB");
		assert_eq!(bytes(999), "999 B");
		assert_eq!(bytes(-5), "0 B");
		assert_eq!(span(time::Duration::seconds(45)), "45 s");
		assert_eq!(span(time::Duration::seconds(720)), "12 min");
		assert_eq!(span(time::Duration::seconds(7300)), "2 h");
		assert_eq!(span(time::Duration::days(3)), "3 d");
	}

	#[test]
	fn the_stat_row_escapes_and_keeps_its_cells() {
		let html = stats("Live figures", &[("CPU", "<b>1</b>".to_owned())]).into_string();
		assert!(html.contains("aria-label=\"Live figures\""), "{html}");
		assert!(html.contains("&lt;b&gt;1&lt;/b&gt;"), "{html}");
	}
}
