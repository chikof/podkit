//! Badge and status. A tag labels a fact (`Tone`); a status shows the state of a thing that
//! runs (`Status`). Neither relies on color alone: both always carry text, and a status
//! also has its own shape (filled, pulsing, ringed, diamond, hollow, dashed, slashed), drawn
//! by `components.css` from `data-status`.

use maud::{Markup, html};

/// How a tag reads at a glance. One color per tone, from `theme.css`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
	/// Idle or finished without a verdict: queued, stopped, cancelled.
	Neutral,
	/// Work in progress.
	Progress,
	/// Healthy or completed.
	Success,
	/// Needs attention but is not broken.
	Warning,
	/// Failed.
	Danger,
}

const NEUTRAL: &str = "ui-tag ui-tag-neutral";
const PROGRESS: &str = "ui-tag ui-tag-progress";
const SUCCESS: &str = "ui-tag ui-tag-success";
const WARNING: &str = "ui-tag ui-tag-warning";
const DANGER: &str = "ui-tag ui-tag-danger";

impl Tone {
	pub const fn badge_class(self) -> &'static str {
		match self {
			Self::Neutral => NEUTRAL,
			Self::Progress => PROGRESS,
			Self::Success => SUCCESS,
			Self::Warning => WARNING,
			Self::Danger => DANGER,
		}
	}
}

pub fn badge(tone: Tone, text: &str) -> Markup {
	html! { span class=(tone.badge_class()) { (text) } }
}

/// The state of something that runs. `Pending` is waiting on something outside podkit
/// (a DNS record, a host key to confirm); `Unreachable` is a host podkit cannot talk to;
/// `Building` and `Deploying` are podkit's own work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
	Running,
	Building,
	Deploying,
	Failed,
	Stopped,
	Pending,
	Unreachable,
}

impl Status {
	pub const ALL: [Self; 7] = [
		Self::Running,
		Self::Building,
		Self::Deploying,
		Self::Failed,
		Self::Stopped,
		Self::Pending,
		Self::Unreachable,
	];

	pub const fn label(self) -> &'static str {
		match self {
			Self::Running => "Running",
			Self::Building => "Building",
			Self::Deploying => "Deploying",
			Self::Failed => "Failed",
			Self::Stopped => "Stopped",
			Self::Pending => "Pending",
			Self::Unreachable => "Unreachable",
		}
	}

	/// The value of `data-status`, which the stylesheet keys its color and shape on.
	pub const fn key(self) -> &'static str {
		match self {
			Self::Running => "running",
			Self::Building => "building",
			Self::Deploying => "deploying",
			Self::Failed => "failed",
			Self::Stopped => "stopped",
			Self::Pending => "pending",
			Self::Unreachable => "unreachable",
		}
	}
}

/// A shaped dot and the status word: for a row, a card or a page header.
pub fn status_badge(status: Status) -> Markup {
	html! {
		span class="ui-badge" data-status=(status.key()) {
			span class="ui-dot" aria-hidden="true" {}
			span { (status.label()) }
		}
	}
}

/// The same, under its older name: a dot and the word.
pub fn status_dot(status: Status) -> Markup {
	status_badge(status)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_badge_escapes_its_text() {
		let html = badge(Tone::Danger, "<script>1</script>").into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("ui-tag-danger"), "{html}");
	}

	#[test]
	fn every_tone_has_its_own_class() {
		let tones = [
			Tone::Neutral,
			Tone::Progress,
			Tone::Success,
			Tone::Warning,
			Tone::Danger,
		];
		for (i, a) in tones.iter().enumerate() {
			for b in &tones[i + 1..] {
				assert_ne!(a.badge_class(), b.badge_class());
			}
		}
	}

	#[test]
	fn a_status_hides_the_dot_and_says_the_word() {
		for status in Status::ALL {
			let html = status_badge(status).into_string();
			assert!(html.contains("aria-hidden=\"true\""), "{html}");
			assert!(html.contains(status.label()), "{html}");
			assert!(
				html.contains(&format!("data-status=\"{}\"", status.key())),
				"{html}"
			);
		}
	}

	#[test]
	fn all_seven_statuses_have_distinct_keys_and_words() {
		assert_eq!(Status::ALL.len(), 7);
		let keys: Vec<_> = Status::ALL.iter().map(|s| s.key()).collect();
		let words: Vec<_> = Status::ALL.iter().map(|s| s.label()).collect();
		for list in [keys, words] {
			let mut sorted = list.clone();
			sorted.sort_unstable();
			sorted.dedup();
			assert_eq!(sorted.len(), 7, "{list:?}");
		}
		for key in [
			"running",
			"building",
			"deploying",
			"failed",
			"stopped",
			"pending",
			"unreachable",
		] {
			assert!(Status::ALL.iter().any(|s| s.key() == key), "{key}");
		}
	}

	#[test]
	fn a_status_badge_is_one_element_with_a_status_hook() {
		let html = status_badge(Status::Unreachable).into_string();
		assert_eq!(html.matches("data-status").count(), 1, "{html}");
		assert!(html.contains("class=\"ui-badge\""), "{html}");
	}
}
