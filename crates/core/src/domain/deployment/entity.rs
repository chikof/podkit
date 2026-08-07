use time::OffsetDateTime;

use crate::domain::shared::errors::{DomainError, DomainResult};
use crate::domain::shared::ids::{ApplicationId, DeploymentId, UserId};

/// Lifecycle state of a single deployment attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentStatus {
	/// Created, not yet picked up by a worker.
	Queued,
	/// Image build in progress.
	Building,
	/// Build finished, container is being created and started.
	Deploying,
	/// Container is up and considered healthy.
	Running,
	/// Build or deploy failed. Terminal.
	Failed,
	/// Was running and was manually stopped. Terminal.
	Stopped,
}

impl DeploymentStatus {
	/// Returns the lowercase string form used for storage and display.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Queued => "queued",
			Self::Building => "building",
			Self::Deploying => "deploying",
			Self::Running => "running",
			Self::Failed => "failed",
			Self::Stopped => "stopped",
		}
	}

	/// Parses the storage string form back into a status. Unrecognized input
	/// falls back to `Queued` rather than failing.
	#[must_use]
	pub fn parse(s: &str) -> Self {
		match s {
			"building" => Self::Building,
			"deploying" => Self::Deploying,
			"running" => Self::Running,
			"failed" => Self::Failed,
			"stopped" => Self::Stopped,
			_ => Self::Queued,
		}
	}
}

/// One attempt to build and run an [`Application`](super::super::application::Application)
/// at a specific commit. A retry after failure creates a *new* row, since
/// deployments are an append-only history of attempts; this type only
/// governs transitions within one attempt's own lifecycle.
#[derive(Debug, Clone)]
pub struct Deployment {
	/// Unique id of this deployment attempt.
	pub id: DeploymentId,
	/// The application being deployed.
	pub application_id: ApplicationId,
	/// Current lifecycle state.
	pub status: DeploymentStatus,
	/// Git commit sha being deployed, once known.
	pub commit_sha: Option<String>,
	/// Tag of the image built for this deployment, once built.
	pub image_tag: Option<String>,
	/// Id of the running container, once created.
	pub container_id: Option<String>,
	/// Failure detail, set when `status` is `Failed`.
	pub error_message: Option<String>,
	/// User who triggered this deployment, if any (vs. e.g. an automated hook).
	pub triggered_by: Option<UserId>,
	/// When the deployment attempt was created.
	pub created_at: OffsetDateTime,
	/// When the build started, once it starts.
	pub started_at: Option<OffsetDateTime>,
	/// When the attempt reached a terminal state.
	pub finished_at: Option<OffsetDateTime>,
}

impl Deployment {
	/// Creates a new deployment attempt in the `Queued` state.
	#[must_use]
	pub fn queued(
		id: DeploymentId,
		application_id: ApplicationId,
		triggered_by: Option<UserId>,
	) -> Self {
		Self {
			id,
			application_id,
			status: DeploymentStatus::Queued,
			commit_sha: None,
			image_tag: None,
			container_id: None,
			error_message: None,
			triggered_by,
			created_at: OffsetDateTime::now_utc(),
			started_at: None,
			finished_at: None,
		}
	}

	/// Enforces the allowed status transitions: `queued -> building ->
	/// deploying -> {running|failed}`, plus `running -> stopped` for a
	/// manual stop. Any other transition (skip, backward, or out of a
	/// terminal state) is rejected.
	///
	/// # Errors
	/// Returns [`DomainError::Validation`] if the transition isn't allowed.
	pub fn transition(&mut self, next: DeploymentStatus) -> DomainResult<()> {
		use DeploymentStatus::{Building, Deploying, Failed, Queued, Running, Stopped};

		let allowed = matches!(
			(self.status, next),
			(Queued, Building)
				| (Building, Deploying | Failed)
				| (Deploying, Running | Failed)
				| (Running, Stopped)
		);

		if !allowed {
			return Err(DomainError::Validation(format!(
				"invalid deployment status transition: {:?} -> {:?}",
				self.status, next
			)));
		}

		self.status = next;
		match next {
			Building => self.started_at = Some(OffsetDateTime::now_utc()),
			Running | Failed | Stopped => self.finished_at = Some(OffsetDateTime::now_utc()),
			Queued | Deploying => {}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn new_deployment() -> Deployment {
		Deployment::queued(DeploymentId(1), ApplicationId(1), Some(UserId(1)))
	}

	#[test]
	fn happy_path_forward_transitions_succeed() {
		let mut d = new_deployment();
		assert!(d.transition(DeploymentStatus::Building).is_ok());
		assert!(d.transition(DeploymentStatus::Deploying).is_ok());
		assert!(d.transition(DeploymentStatus::Running).is_ok());
		assert_eq!(d.status, DeploymentStatus::Running);
		assert!(d.finished_at.is_some());
	}

	#[test]
	fn build_failure_shortcuts_to_failed() {
		let mut d = new_deployment();
		d.transition(DeploymentStatus::Building).unwrap();
		assert!(d.transition(DeploymentStatus::Failed).is_ok());
		assert_eq!(d.status, DeploymentStatus::Failed);
	}

	#[test]
	fn skipping_a_state_is_rejected() {
		let mut d = new_deployment();
		assert!(d.transition(DeploymentStatus::Deploying).is_err());
		assert_eq!(
			d.status,
			DeploymentStatus::Queued,
			"rejected transition must not mutate state"
		);
	}

	#[test]
	fn backward_transition_is_rejected() {
		let mut d = new_deployment();
		d.transition(DeploymentStatus::Building).unwrap();
		d.transition(DeploymentStatus::Deploying).unwrap();
		d.transition(DeploymentStatus::Running).unwrap();
		assert!(d.transition(DeploymentStatus::Building).is_err());
	}

	#[test]
	fn transition_out_of_terminal_failed_is_rejected() {
		let mut d = new_deployment();
		d.transition(DeploymentStatus::Building).unwrap();
		d.transition(DeploymentStatus::Failed).unwrap();
		assert!(d.transition(DeploymentStatus::Deploying).is_err());
	}

	#[test]
	fn running_can_be_stopped() {
		let mut d = new_deployment();
		d.transition(DeploymentStatus::Building).unwrap();
		d.transition(DeploymentStatus::Deploying).unwrap();
		d.transition(DeploymentStatus::Running).unwrap();
		assert!(d.transition(DeploymentStatus::Stopped).is_ok());
	}
}
