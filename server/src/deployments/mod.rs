//! Deployments: one append-only row per attempt to build and run an
//! application, its status state machine, and the queries over the
//! `deployments` table.

mod routes;
pub mod run;
pub mod service;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::applications::Application;
use crate::error::ServerError;
use crate::ids::{ApplicationId, DeploymentId, UserId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;

pub use routes::{DeploymentResponse, router};

/// Lifecycle state of a single deployment attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
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
	/// Was running and was manually stopped, or replaced by a newer deploy. Terminal.
	Stopped,
	/// Was running until the container exited and the agent gave up restarting it.
	Crashed,
	/// Cancelled while queued or in progress. Terminal.
	Cancelled,
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
			Self::Crashed => "crashed",
			Self::Cancelled => "cancelled",
		}
	}

	/// Allowed transitions: `queued -> building -> deploying -> running`;
	/// `queued`, `building` and `deploying` may also fail or be cancelled;
	/// `running -> {stopped|crashed}`; `crashed -> stopped`. Anything else
	/// (skip, backward, or out of a terminal state) is rejected.
	#[must_use]
	pub fn can_transition_to(self, next: Self) -> bool {
		use DeploymentStatus::{
			Building, Cancelled, Crashed, Deploying, Failed, Queued, Running, Stopped,
		};

		matches!(
			(self, next),
			(Queued, Building | Failed | Cancelled)
				| (Building, Deploying | Failed | Cancelled)
				| (Deploying, Running | Failed | Cancelled)
				| (Running, Stopped | Crashed)
				| (Crashed, Stopped)
		)
	}
}

/// A status transition the state machine does not allow.
#[derive(Debug, thiserror::Error)]
#[error("invalid deployment status transition: {from:?} -> {to:?}")]
pub struct InvalidTransition {
	/// Status the deployment was in.
	pub from: DeploymentStatus,
	/// Status that was requested.
	pub to: DeploymentStatus,
}

/// One attempt to build and run an application at a specific commit. A
/// retry after failure creates a *new* row, since deployments are an
/// append-only history of attempts; the state machine only governs
/// transitions within one attempt's own lifecycle.
#[derive(Debug, Clone, sqlx::FromRow)]
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
	/// When the build started, once it starts.
	pub started_at: Option<OffsetDateTime>,
	/// When the attempt reached a terminal state.
	pub finished_at: Option<OffsetDateTime>,
}

impl Deployment {
	/// Moves to `next` if [`DeploymentStatus::can_transition_to`] allows it,
	/// stamping `started_at`/`finished_at`. A rejected transition leaves the
	/// deployment untouched.
	///
	/// # Errors
	/// Returns [`InvalidTransition`] if the transition isn't allowed.
	pub fn transition(&mut self, next: DeploymentStatus) -> Result<(), InvalidTransition> {
		use DeploymentStatus::{
			Building, Cancelled, Crashed, Deploying, Failed, Queued, Running, Stopped,
		};

		if !self.status.can_transition_to(next) {
			return Err(InvalidTransition {
				from: self.status,
				to: next,
			});
		}

		self.status = next;
		match next {
			Building => self.started_at = Some(OffsetDateTime::now_utc()),
			Running | Failed | Stopped | Crashed | Cancelled => {
				self.finished_at = Some(OffsetDateTime::now_utc());
			}
			Queued | Deploying => {}
		}

		Ok(())
	}
}

/// Inserts a new deployment row in the `queued` state. `commit_sha` and
/// `image_tag` are pre-filled only for restarts and rollbacks, which reuse
/// an already-built image instead of building from source.
pub async fn insert(
	db: impl PgExecutor<'_>,
	application_id: ApplicationId,
	triggered_by: Option<UserId>,
	commit_sha: Option<&str>,
	image_tag: Option<&str>,
) -> Result<Deployment, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			INSERT INTO deployments (id, application_id, triggered_by, commit_sha, image_tag)
			VALUES ($1, $2, $3, $4, $5)
			RETURNING
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
		"#,
		DeploymentId::generate().0,
		application_id.0,
		triggered_by.map(|u| u.0),
		commit_sha,
		image_tag,
	)
	.fetch_one(db)
	.await
}

/// Inserts a queued deployment for `app` and enqueues its Deploy job, which
/// is the single entry point for deploy, restart, rollback and webhook.
/// With `reuse`, the new row carries that deployment's image and commit so
/// the job redeploys it without cloning or building.
///
/// # Errors
/// [`ServerError::Validation`] when `reuse` never produced an image, or a
/// database error.
pub async fn queue(
	state: &AppState,
	app: &Application,
	triggered_by: Option<UserId>,
	reuse: Option<&Deployment>,
) -> Result<Deployment, ServerError> {
	let image_tag = reuse
		.map(|t| {
			t.image_tag.as_deref().ok_or_else(|| {
				ServerError::Validation(
					"target deployment never produced an image to redeploy".to_string(),
				)
			})
		})
		.transpose()?;
	let commit_sha = reuse.and_then(|t| t.commit_sha.as_deref());

	let deployment = insert(&state.db, app.id, triggered_by, commit_sha, image_tag).await?;
	if let Err(e) = jobs::enqueue(
		state,
		JobKind::Deploy,
		app.server_id,
		deployment.id.0,
		format!("app:{}", app.id),
	)
	.await
	{
		if let Err(mark) = set_status(
			&state.db,
			deployment.id,
			DeploymentStatus::Failed,
			Some("failed to queue deploy job"),
		)
		.await
		{
			tracing::warn!(
				deployment = deployment.id.0,
				"failed to mark unqueued deployment failed: {mark}"
			);
		}
		return Err(e.into());
	}
	if triggered_by.is_none()
		&& let Some(ctx) = crate::audit::system_in_project(&state.db, app.project_id).await
	{
		crate::audit::record(
			&state.db,
			&ctx,
			if reuse.is_some() {
				"app.rollback"
			} else {
				"app.deploy"
			},
			crate::audit::Target::new("application", app.id)
				.named(&app.slug)
				.in_project(app.project_id),
			crate::audit::Diff::new()
				.set("deployment_id", deployment.id.0)
				.set("trigger", "push"),
		)
		.await;
	}
	Ok(deployment)
}

/// Persists the mutable fields of `d` (status plus what changes alongside
/// it) as one attempt progresses through its own lifecycle.
pub async fn update(db: impl PgExecutor<'_>, d: &Deployment) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			UPDATE deployments SET
				status = $2,
				commit_sha = $3,
				image_tag = $4,
				container_id = $5,
				error_message = $6,
				started_at = $7,
				finished_at = $8
			WHERE id = $1
		"#,
		d.id.0,
		d.status.as_str(),
		d.commit_sha,
		d.image_tag,
		d.container_id,
		d.error_message,
		d.started_at,
		d.finished_at,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Sets a status (and optionally an error message) on one row without
/// loading it first, stamping `started_at`/`finished_at` the way
/// [`Deployment::transition`] does. Does not check the state machine; the
/// caller decides whether the move is legal. Returns the updated row.
pub async fn set_status(
	db: impl PgExecutor<'_>,
	id: DeploymentId,
	status: DeploymentStatus,
	error_message: Option<&str>,
) -> Result<Deployment, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			UPDATE deployments SET
				status = $2::varchar,
				error_message = COALESCE($3, error_message),
				started_at = CASE WHEN $2::varchar = 'building' THEN NOW() ELSE started_at END,
				finished_at = CASE
					WHEN $2::varchar IN ('running', 'failed', 'stopped', 'crashed', 'cancelled') THEN NOW()
					ELSE finished_at
				END
			WHERE id = $1
			RETURNING
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
		"#,
		id.0,
		status.as_str(),
		error_message,
	)
	.fetch_one(db)
	.await
}

/// Looks up a deployment by id alone, with no application or team check.
/// Only for the job runner, which gets the id from a job row it created.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: DeploymentId,
) -> Result<Option<Deployment>, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			SELECT
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
			FROM deployments
			WHERE id = $1
		"#,
		id.0,
	)
	.fetch_optional(db)
	.await
}

/// Looks up a deployment by id, scoped to its application so a deployment
/// id from another application never resolves.
pub async fn find(
	db: impl PgExecutor<'_>,
	application_id: ApplicationId,
	id: DeploymentId,
) -> Result<Option<Deployment>, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			SELECT
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
			FROM deployments
			WHERE id = $2 AND application_id = $1
		"#,
		application_id.0,
		id.0,
	)
	.fetch_optional(db)
	.await
}

/// Lists an application's deployments, newest first. `before` is the
/// keyset cursor: only ids strictly below it are returned.
pub async fn list(
	db: impl PgExecutor<'_>,
	application_id: ApplicationId,
	before: Option<DeploymentId>,
	limit: i64,
) -> Result<Vec<Deployment>, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			SELECT
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
			FROM deployments
			WHERE application_id = $1 AND ($2::BIGINT IS NULL OR id < $2)
			ORDER BY id DESC
			LIMIT $3
		"#,
		application_id.0,
		before.map(|b| b.0),
		limit,
	)
	.fetch_all(db)
	.await
}

/// The restart target: the newest deployment that produced an image.
pub async fn latest_with_image(
	db: impl PgExecutor<'_>,
	application_id: ApplicationId,
) -> Result<Option<Deployment>, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			SELECT
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
			FROM deployments
			WHERE application_id = $1 AND image_tag IS NOT NULL
			ORDER BY id DESC
			LIMIT 1
		"#,
		application_id.0,
	)
	.fetch_optional(db)
	.await
}

/// The application's newest deployment still marked `running`.
pub async fn latest_running(
	db: impl PgExecutor<'_>,
	application_id: ApplicationId,
) -> Result<Option<Deployment>, sqlx::Error> {
	sqlx::query_as!(
		Deployment,
		r#"
			SELECT
				id AS "id!: DeploymentId",
				application_id AS "application_id!: ApplicationId",
				status AS "status!: DeploymentStatus",
				commit_sha, image_tag, container_id, error_message,
				started_at, finished_at
			FROM deployments
			WHERE application_id = $1 AND status = 'running'
			ORDER BY id DESC
			LIMIT 1
		"#,
		application_id.0,
	)
	.fetch_optional(db)
	.await
}

/// Marks every other `running` or `crashed` deployment of the application
/// `stopped`: the one that just went live replaced them.
pub async fn retire_others(
	db: impl PgExecutor<'_>,
	application_id: ApplicationId,
	current: DeploymentId,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE deployments SET status = 'stopped', finished_at = NOW()
		WHERE application_id = $1 AND id <> $2 AND status IN ('running', 'crashed')",
		application_id.0,
		current.0,
	)
	.execute(db)
	.await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn new_deployment() -> Deployment {
		Deployment {
			id: DeploymentId(1),
			application_id: ApplicationId(1),
			status: DeploymentStatus::Queued,
			commit_sha: None,
			image_tag: None,
			container_id: None,
			error_message: None,
			started_at: None,
			finished_at: None,
		}
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

	#[test]
	fn in_progress_states_can_be_cancelled() {
		for steps in [
			&[][..],
			&[DeploymentStatus::Building][..],
			&[DeploymentStatus::Building, DeploymentStatus::Deploying][..],
		] {
			let mut d = new_deployment();
			for s in steps {
				d.transition(*s).unwrap();
			}
			assert!(d.transition(DeploymentStatus::Cancelled).is_ok());
			assert!(d.finished_at.is_some());
		}
	}

	#[test]
	fn queued_can_fail_without_building() {
		let mut d = new_deployment();
		assert!(d.transition(DeploymentStatus::Failed).is_ok());
	}

	#[test]
	fn running_can_crash_and_a_crash_can_be_stopped() {
		let mut d = new_deployment();
		d.transition(DeploymentStatus::Building).unwrap();
		d.transition(DeploymentStatus::Deploying).unwrap();
		d.transition(DeploymentStatus::Running).unwrap();
		assert!(d.transition(DeploymentStatus::Crashed).is_ok());
		assert!(d.transition(DeploymentStatus::Running).is_err());
		assert!(d.transition(DeploymentStatus::Stopped).is_ok());
	}

	#[test]
	fn terminal_states_have_no_exits() {
		use DeploymentStatus::{
			Building, Cancelled, Crashed, Deploying, Failed, Queued, Running, Stopped,
		};
		let all = [
			Queued, Building, Deploying, Running, Failed, Stopped, Crashed, Cancelled,
		];
		for from in [Failed, Stopped, Cancelled] {
			for to in all {
				assert!(!from.can_transition_to(to), "{from:?} -> {to:?}");
			}
		}
		assert!(!Running.can_transition_to(Failed));
		assert!(!Running.can_transition_to(Cancelled));
		assert!(!Crashed.can_transition_to(Failed));
	}
}
