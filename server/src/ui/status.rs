//! Status badges and background polling. Every status enum implements
//! [`Status`] with exhaustive matches (no wildcard arm), so adding a variant
//! fails to compile until it has a label and a tone.
//!
//! This is not `components.rs` on purpose: `build.rs` treats every string
//! literal there as a class list, and the labels here are not.

use maud::{Markup, html};

use super::components::Tone;
use super::sse::js_string;
use crate::backups::BackupStatus;
use crate::databases::DatabaseStatus;
use crate::deployments::DeploymentStatus;
use crate::model_files::ModelFileStatus;
use crate::models::ModelStatus;
use crate::servers::ServerStatus;
use crate::stacks::{ServiceStatus, StackStatus};

pub trait Status: Copy {
	fn label(self) -> &'static str;
	fn tone(self) -> Tone;
	/// True while the status is expected to change by itself, so a page
	/// showing it should keep [`poll`]ing.
	fn in_flight(self) -> bool;
}

impl Status for DeploymentStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Queued => "Queued",
			Self::Building => "Building",
			Self::Deploying => "Deploying",
			Self::Running => "Running",
			Self::Failed => "Failed",
			Self::Stopped => "Stopped",
			Self::Crashed => "Crashed",
			Self::Cancelled => "Cancelled",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Queued | Self::Stopped | Self::Cancelled => Tone::Neutral,
			Self::Building | Self::Deploying => Tone::Progress,
			Self::Running => Tone::Success,
			Self::Failed | Self::Crashed => Tone::Danger,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Queued | Self::Building | Self::Deploying => true,
			Self::Running | Self::Failed | Self::Stopped | Self::Crashed | Self::Cancelled => false,
		}
	}
}

impl Status for ServerStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Pending => "Pending",
			Self::Active => "Active",
			Self::Unreachable => "Unreachable",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Pending => Tone::Progress,
			Self::Active => Tone::Success,
			Self::Unreachable => Tone::Danger,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Pending => true,
			Self::Active | Self::Unreachable => false,
		}
	}
}

impl Status for DatabaseStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Pending => "Pending",
			Self::Running => "Running",
			Self::Stopped => "Stopped",
			Self::Failed => "Failed",
			Self::Deleting => "Deleting",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Pending => Tone::Progress,
			Self::Running => Tone::Success,
			Self::Stopped => Tone::Neutral,
			Self::Failed => Tone::Danger,
			Self::Deleting => Tone::Warning,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Pending | Self::Deleting => true,
			Self::Running | Self::Stopped | Self::Failed => false,
		}
	}
}

impl Status for BackupStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Queued => "Queued",
			Self::Running => "Running",
			Self::Succeeded => "Succeeded",
			Self::Failed => "Failed",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Queued => Tone::Neutral,
			Self::Running => Tone::Progress,
			Self::Succeeded => Tone::Success,
			Self::Failed => Tone::Danger,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Queued | Self::Running => true,
			Self::Succeeded | Self::Failed => false,
		}
	}
}

impl Status for ModelStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Starting => "Starting",
			Self::Running => "Running",
			Self::Stopped => "Stopped",
			Self::Failed => "Failed",
			Self::Removing => "Removing",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Starting => Tone::Progress,
			Self::Running => Tone::Success,
			Self::Stopped => Tone::Neutral,
			Self::Failed => Tone::Danger,
			Self::Removing => Tone::Warning,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Starting | Self::Removing => true,
			Self::Running | Self::Stopped | Self::Failed => false,
		}
	}
}

impl Status for ModelFileStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Queued => "Queued",
			Self::Downloading => "Downloading",
			Self::Ready => "Ready",
			Self::Failed => "Failed",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Queued => Tone::Neutral,
			Self::Downloading => Tone::Progress,
			Self::Ready => Tone::Success,
			Self::Failed => Tone::Danger,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Queued | Self::Downloading => true,
			Self::Ready | Self::Failed => false,
		}
	}
}

impl Status for StackStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Pending => "Not deployed",
			Self::Deploying => "Deploying",
			Self::Running => "Running",
			Self::Stopped => "Stopped",
			Self::Failed => "Failed",
			Self::Deleting => "Deleting",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Pending | Self::Stopped => Tone::Neutral,
			Self::Deploying => Tone::Progress,
			Self::Running => Tone::Success,
			Self::Failed => Tone::Danger,
			Self::Deleting => Tone::Warning,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Deploying | Self::Deleting => true,
			Self::Pending | Self::Running | Self::Stopped | Self::Failed => false,
		}
	}
}

impl Status for ServiceStatus {
	fn label(self) -> &'static str {
		match self {
			Self::Pending => "Not deployed",
			Self::Running => "Running",
			Self::Stopped => "Stopped",
			Self::Failed => "Failed",
			Self::Removing => "Removing",
		}
	}

	fn tone(self) -> Tone {
		match self {
			Self::Pending | Self::Stopped => Tone::Neutral,
			Self::Running => Tone::Success,
			Self::Failed => Tone::Danger,
			Self::Removing => Tone::Warning,
		}
	}

	fn in_flight(self) -> bool {
		match self {
			Self::Removing => true,
			Self::Pending | Self::Running | Self::Stopped | Self::Failed => false,
		}
	}
}

/// A pill showing `status` in its tone's color.
pub fn status_badge(status: impl Status) -> Markup {
	html! {
		span class=(status.tone().badge_class()) { (status.label()) }
	}
}

/// Re-fetches `url` every 3 seconds while `active`, and renders nothing once
/// it is not. The response should re-render the region that contains this
/// call, so a terminal state drops the element and polling stops by itself.
pub fn poll(url: &str, active: bool) -> Markup {
	html! {
		@if active {
			div hidden "data-on-interval__duration.3s"=(format!("@get({})", js_string(url))) {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn all_deployment() -> [DeploymentStatus; 8] {
		use DeploymentStatus::*;
		[
			Queued, Building, Deploying, Running, Failed, Stopped, Crashed, Cancelled,
		]
	}

	#[test]
	fn every_variant_has_a_label_and_badge() {
		for s in all_deployment() {
			assert!(status_badge(s).0.contains(s.label()));
		}
		for s in [
			ServerStatus::Pending,
			ServerStatus::Active,
			ServerStatus::Unreachable,
		] {
			assert!(status_badge(s).0.contains(s.label()));
		}
		for s in [
			DatabaseStatus::Pending,
			DatabaseStatus::Running,
			DatabaseStatus::Stopped,
			DatabaseStatus::Failed,
			DatabaseStatus::Deleting,
		] {
			assert!(status_badge(s).0.contains(s.label()));
		}
		for s in [
			BackupStatus::Queued,
			BackupStatus::Running,
			BackupStatus::Succeeded,
			BackupStatus::Failed,
		] {
			assert!(status_badge(s).0.contains(s.label()));
		}
	}

	#[test]
	fn tones_separate_healthy_busy_and_broken() {
		assert_eq!(DeploymentStatus::Running.tone(), Tone::Success);
		assert_eq!(DeploymentStatus::Building.tone(), Tone::Progress);
		assert_eq!(DeploymentStatus::Failed.tone(), Tone::Danger);
		assert_ne!(
			status_badge(DeploymentStatus::Running).0,
			status_badge(DeploymentStatus::Failed).0
		);
	}

	#[test]
	fn polling_only_while_a_state_can_still_change() {
		let on = poll("/x?a=1&b='", true).0;
		assert!(on.contains("data-on-interval__duration.3s"), "{on}");
		assert!(on.contains("@get('/x?a=1&amp;b=\\u0027')"), "{on}");
		assert_eq!(poll("/x", false).0, "");

		for s in all_deployment() {
			let terminal = matches!(
				s,
				DeploymentStatus::Running
					| DeploymentStatus::Failed
					| DeploymentStatus::Stopped
					| DeploymentStatus::Crashed
					| DeploymentStatus::Cancelled
			);
			assert_eq!(poll("/x", s.in_flight()).0.is_empty(), terminal, "{s:?}");
		}
		assert!(BackupStatus::Running.in_flight() && !BackupStatus::Succeeded.in_flight());
		assert!(ServerStatus::Pending.in_flight() && !ServerStatus::Active.in_flight());
		assert!(DatabaseStatus::Deleting.in_flight() && !DatabaseStatus::Stopped.in_flight());
	}
}
