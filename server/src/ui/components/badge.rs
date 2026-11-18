//! Badge and status dot. A badge labels a fact (`Tone`); a status dot shows the state of
//! a thing that runs (`Status`). Neither relies on color alone: both always carry text.

use maud::{Markup, html};

/// How a badge reads at a glance. One color per tone, from `theme.css`.
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

const NEUTRAL: &str = "inline-flex items-center rounded-full border border-border px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-muted-foreground";
const PROGRESS: &str = "inline-flex items-center rounded-full border border-info/30 bg-info/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-info";
const SUCCESS: &str = "inline-flex items-center rounded-full border border-success/30 bg-success/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-success";
const WARNING: &str = "inline-flex items-center rounded-full border border-warning/30 bg-warning/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-warning";
const DANGER: &str = "inline-flex items-center rounded-full border border-destructive/30 bg-destructive/15 px-2.5 py-0.5 text-xs font-medium whitespace-nowrap text-destructive";

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
/// (a DNS record, a host key to confirm); `Building` and the like are podkit's own work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
	Running,
	Building,
	Failed,
	Stopped,
	Pending,
}

const DOT_RUNNING: &str = "ui-dot ui-dot-success";
const DOT_BUILDING: &str = "ui-dot ui-dot-progress";
const DOT_FAILED: &str = "ui-dot ui-dot-danger";
const DOT_STOPPED: &str = "ui-dot ui-dot-neutral";
const DOT_PENDING: &str = "ui-dot ui-dot-warning";

impl Status {
	pub const fn label(self) -> &'static str {
		match self {
			Self::Running => "Running",
			Self::Building => "Building",
			Self::Failed => "Failed",
			Self::Stopped => "Stopped",
			Self::Pending => "Pending",
		}
	}

	const fn dot_class(self) -> &'static str {
		match self {
			Self::Running => DOT_RUNNING,
			Self::Building => DOT_BUILDING,
			Self::Failed => DOT_FAILED,
			Self::Stopped => DOT_STOPPED,
			Self::Pending => DOT_PENDING,
		}
	}

	/// The badge tone that matches this status.
	pub const fn tone(self) -> Tone {
		match self {
			Self::Running => Tone::Success,
			Self::Building => Tone::Progress,
			Self::Failed => Tone::Danger,
			Self::Stopped => Tone::Neutral,
			Self::Pending => Tone::Warning,
		}
	}
}

/// A colored dot and the status word: for a row or a card.
pub fn status_dot(status: Status) -> Markup {
	html! {
		span class="inline-flex items-center gap-2 text-sm" data-status=(status.label()) {
			span class=(status.dot_class()) aria-hidden="true" {}
			(status.label())
		}
	}
}

/// The same status as a badge with a dot, for a page header.
pub fn status_badge(status: Status) -> Markup {
	html! {
		span class={ (status.tone().badge_class()) " gap-1.5" } data-status=(status.label()) {
			span class=(status.dot_class()) aria-hidden="true" {}
			(status.label())
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const ALL: [Status; 5] = [
		Status::Running,
		Status::Building,
		Status::Failed,
		Status::Stopped,
		Status::Pending,
	];

	#[test]
	fn a_badge_escapes_its_text() {
		let html = badge(Tone::Danger, "<script>1</script>").into_string();
		assert!(!html.contains("<script>"), "{html}");
		assert!(html.contains("text-destructive"), "{html}");
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
	fn a_status_dot_hides_the_dot_and_says_the_word() {
		for status in ALL {
			let html = status_dot(status).into_string();
			assert!(html.contains("aria-hidden=\"true\""), "{html}");
			assert!(html.contains(status.label()), "{html}");
		}
	}

	#[test]
	fn each_status_has_a_distinct_dot_and_a_matching_badge_tone() {
		let dots: Vec<_> = ALL.iter().map(|s| s.dot_class()).collect();
		for (i, a) in dots.iter().enumerate() {
			for b in &dots[i + 1..] {
				assert_ne!(a, b);
			}
		}
		assert_eq!(Status::Running.tone(), Tone::Success);
		assert_eq!(Status::Failed.tone(), Tone::Danger);
		assert!(
			status_badge(Status::Building)
				.into_string()
				.contains("text-info")
		);
	}
}
