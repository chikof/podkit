//! Deployment use cases shared by the JSON API and the web UI: authorize,
//! then scope, then validate, then touch the database or the agent.

use axum::http::Method;
use futures_util::stream::BoxStream;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{Deployment, DeploymentStatus};
use crate::applications::Application;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::deployments;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DeploymentId, JobId, ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::scope;
use crate::state::AppState;

/// The most container log lines one request may replay.
pub const MAX_TAIL: u32 = 5000;

/// Records a deployment action on the application it belongs to.
async fn log(
	s: &AppState,
	user: UserId,
	team: TeamId,
	app: &Application,
	action: &str,
	d: &Deployment,
) {
	audit::log(
		&s.db,
		user,
		team,
		action,
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(app.project_id),
		Diff::new()
			.set("deployment_id", d.id.0)
			.set("status", d.status.as_str()),
	)
	.await;
}

/// What started a deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
	/// Someone clicked Deploy, Restart or Rollback.
	User,
	/// A push to the git provider, which has no acting user.
	Webhook,
	/// The health watch put the previous image back after a release went bad.
	AutoRollback,
}

/// What the health gate recorded about a deployment.
#[derive(Debug, Clone)]
pub struct GateView {
	pub state: crate::health_gate::State,
	pub detail: Option<String>,
}

/// A deployment as the UI lists it: its row plus who started it and when.
#[derive(Debug, Clone)]
pub struct DeploymentView {
	pub id: DeploymentId,
	pub status: DeploymentStatus,
	pub commit_sha: Option<String>,
	/// Whether a built image exists, which is what a rollback needs.
	pub has_image: bool,
	pub error_message: Option<String>,
	pub trigger: Trigger,
	/// The health gate's verdict; `None` when the application has no gate.
	pub gate: Option<GateView>,
	/// The deployment an automatic rollback replaced.
	pub rollback_of: Option<DeploymentId>,
	pub created_at: OffsetDateTime,
	pub started_at: Option<OffsetDateTime>,
	pub finished_at: Option<OffsetDateTime>,
}

impl DeploymentView {
	/// Queued, building or deploying: it can still change and can be cancelled.
	#[must_use]
	pub fn in_flight(&self) -> bool {
		matches!(
			self.status,
			DeploymentStatus::Queued | DeploymentStatus::Building | DeploymentStatus::Deploying
		)
	}

	/// Cancel is offered while the build has not finished.
	#[must_use]
	pub fn can_cancel(&self) -> bool {
		matches!(
			self.status,
			DeploymentStatus::Queued | DeploymentStatus::Building
		)
	}

	/// Rolling back to a deployment that built an image and is not what is
	/// live now (or still on its way).
	#[must_use]
	pub fn can_rollback(&self) -> bool {
		self.has_image && !self.in_flight() && self.status != DeploymentStatus::Running
	}

	/// The new container runs and the deploy waits for its health check; the old one serves.
	#[must_use]
	pub fn waiting_for_health(&self) -> bool {
		self.in_flight()
			&& self
				.gate
				.as_ref()
				.is_some_and(|g| g.state == crate::health_gate::State::Waiting)
	}

	/// The release went live, failed, and the previous image was put back.
	#[must_use]
	pub fn rolled_back(&self) -> bool {
		self.gate
			.as_ref()
			.is_some_and(|g| g.state == crate::health_gate::State::RolledBack)
	}

	/// How long the build and start took, once it has finished.
	#[must_use]
	pub fn duration(&self) -> Option<time::Duration> {
		Some(self.finished_at? - self.started_at?)
	}
}

#[derive(sqlx::FromRow)]
struct Row {
	id: DeploymentId,
	status: DeploymentStatus,
	commit_sha: Option<String>,
	image_tag: Option<String>,
	error_message: Option<String>,
	triggered_by: Option<i64>,
	created_at: OffsetDateTime,
	started_at: Option<OffsetDateTime>,
	finished_at: Option<OffsetDateTime>,
	health_state: Option<crate::health_gate::State>,
	health_detail: Option<String>,
	rollback_of: Option<i64>,
}

impl From<Row> for DeploymentView {
	fn from(r: Row) -> Self {
		Self {
			id: r.id,
			status: r.status,
			commit_sha: r.commit_sha,
			has_image: r.image_tag.is_some(),
			error_message: r.error_message,
			trigger: if r.triggered_by.is_some() {
				Trigger::User
			} else if r.rollback_of.is_some() {
				Trigger::AutoRollback
			} else {
				Trigger::Webhook
			},
			gate: r.health_state.map(|state| GateView {
				state,
				detail: r.health_detail,
			}),
			rollback_of: r.rollback_of.map(DeploymentId),
			created_at: r.created_at,
			started_at: r.started_at,
			finished_at: r.finished_at,
		}
	}
}

async fn application(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	action: Action,
	resource: Resource,
) -> AppResult<Application> {
	authorize(db, user, team, action, resource).await?;
	scope::application(db, team, project, app).await
}

/// The application's deployments, newest first (the latest 50); needs
/// `read deployment`.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<DeploymentView>> {
	let app = application(
		db,
		user,
		team,
		project,
		app,
		Action::Read,
		Resource::Deployment,
	)
	.await?;
	let rows = sqlx::query_as::<_, Row>(
		"SELECT d.id, d.status, d.commit_sha, d.image_tag, d.error_message, d.triggered_by, d.created_at, d.started_at, d.finished_at, h.state AS health_state, h.detail AS health_detail, h.rollback_of FROM deployments d LEFT JOIN deployment_health h ON h.deployment_id = d.id WHERE d.application_id = $1 ORDER BY d.id DESC LIMIT 50",
	)
	.bind(app.id.0)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(Into::into).collect())
}

/// One deployment of the application; needs `read deployment`.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: DeploymentId,
) -> AppResult<DeploymentView> {
	let app = application(
		db,
		user,
		team,
		project,
		app,
		Action::Read,
		Resource::Deployment,
	)
	.await?;
	sqlx::query_as::<_, Row>(
		"SELECT d.id, d.status, d.commit_sha, d.image_tag, d.error_message, d.triggered_by, d.created_at, d.started_at, d.finished_at, h.state AS health_state, h.detail AS health_detail, h.rollback_of FROM deployments d LEFT JOIN deployment_health h ON h.deployment_id = d.id WHERE d.id = $1 AND d.application_id = $2",
	)
	.bind(id.0)
	.bind(app.id.0)
	.fetch_optional(db)
	.await?
	.map(Into::into)
	.ok_or(ServerError::DeploymentNotFound)
}

/// Queues a deploy of the application's source; needs `create deployment`.
pub async fn deploy(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Deployment> {
	let app = application(
		&s.db,
		user,
		team,
		project,
		app,
		Action::Create,
		Resource::Deployment,
	)
	.await?;
	let d = deployments::queue(s, &app, Some(user), None).await?;
	log(s, user, team, &app, "app.deploy", &d).await;
	Ok(d)
}

/// Redeploys `id`'s built image as a new deployment; needs `create deployment`.
pub async fn rollback(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: DeploymentId,
) -> AppResult<Deployment> {
	let app = application(
		&s.db,
		user,
		team,
		project,
		app,
		Action::Create,
		Resource::Deployment,
	)
	.await?;
	let target = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;
	let deployment = deployments::queue(s, &app, Some(user), Some(&target)).await?;
	log(s, user, team, &app, "app.rollback", &deployment).await;
	crate::notifications::hooks::rollback(s, app.id.0, None).await;
	Ok(deployment)
}

/// Cancels a deployment that is queued, building or deploying; needs
/// `update deployment`.
pub async fn cancel(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: DeploymentId,
) -> AppResult<Deployment> {
	let app = application(
		&s.db,
		user,
		team,
		project,
		app,
		Action::Update,
		Resource::Deployment,
	)
	.await?;
	let d = deployments::find(&s.db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;

	let cancelled = match jobs::latest_for(&s.db, JobKind::Deploy, d.id.0).await? {
		Some(job) => jobs::worker::cancel(s, job.id).await?,
		None => false,
	};
	if !cancelled {
		return Err(ServerError::Validation(
			"deployment is not in progress".to_string(),
		));
	}
	let after = deployments::find(&s.db, app.id, id).await?.unwrap_or(d);
	log(s, user, team, &app, "deployment.cancel", &after).await;
	Ok(after)
}

/// The job whose log is the deployment's build output, if it has one; needs
/// `read deployment`.
pub async fn build_log_job(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	id: DeploymentId,
) -> AppResult<Option<JobId>> {
	let app = application(
		db,
		user,
		team,
		project,
		app,
		Action::Read,
		Resource::Deployment,
	)
	.await?;
	let d = deployments::find(db, app.id, id)
		.await?
		.ok_or(ServerError::DeploymentNotFound)?;
	Ok(jobs::latest_for(db, JobKind::Deploy, d.id.0)
		.await?
		.map(|j| j.id))
}

/// Stops the running deployment's container: a manual halt. The agent stops
/// it before the row moves to `stopped`, so a failed stop can be retried.
/// Needs `update deployment`.
pub async fn stop(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Deployment> {
	let app = application(
		&s.db,
		user,
		team,
		project,
		app,
		Action::Update,
		Resource::Deployment,
	)
	.await?;

	let current = deployments::latest_running(&s.db, app.id)
		.await?
		.ok_or_else(|| {
			ServerError::Validation("application is not currently running".to_string())
		})?;
	let Some(container_id) = current.container_id.as_deref() else {
		return Err(ServerError::Validation(
			"running deployment has no container recorded".to_string(),
		));
	};

	let server = scope::server(&s.db, team, app.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	client
		.send(Method::POST, &format!("/v1/containers/{container_id}/stop"))
		.await?;

	let stopped =
		deployments::set_status(&s.db, current.id, DeploymentStatus::Stopped, None).await?;
	log(s, user, team, &app, "app.stop", &stopped).await;
	Ok(stopped)
}

/// Brings the application back by redeploying its newest built image; no
/// clone or rebuild. Needs `create deployment`, like rollback.
///
/// # Errors
/// [`ServerError::Validation`] if the application never produced a build.
pub async fn restart(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Deployment> {
	let app = application(
		&s.db,
		user,
		team,
		project,
		app,
		Action::Create,
		Resource::Deployment,
	)
	.await?;

	// Newest build with an image, whatever its state now: a later attempt
	// may have failed while an earlier one still has a usable image, and
	// restarting a healthy app is a valid "just in case".
	let target = deployments::latest_with_image(&s.db, app.id)
		.await?
		.ok_or_else(|| {
			ServerError::Validation(
				"no previous successful build to restart from, trigger a deploy instead"
					.to_string(),
			)
		})?;
	let d = deployments::queue(s, &app, Some(user), Some(&target)).await?;
	log(s, user, team, &app, "app.restart", &d).await;
	Ok(d)
}

/// Whether a deployment of the application is running now; needs
/// `read application`.
pub async fn is_running(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<bool> {
	let app = application(
		db,
		user,
		team,
		project,
		app,
		Action::Read,
		Resource::Application,
	)
	.await?;
	Ok(deployments::latest_running(db, app.id).await?.is_some())
}

/// Opens the running container's log stream on its agent; needs
/// `read application`. `tail` is capped at [`MAX_TAIL`].
///
/// # Errors
/// [`ServerError::Validation`] if nothing is running.
pub async fn runtime_log(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	tail: u32,
	follow: bool,
) -> AppResult<BoxStream<'static, Result<proto::Event, ServerError>>> {
	let app = application(
		&s.db,
		user,
		team,
		project,
		app,
		Action::Read,
		Resource::Application,
	)
	.await?;

	let current = deployments::latest_running(&s.db, app.id)
		.await?
		.ok_or_else(|| {
			ServerError::Validation("application is not currently running".to_string())
		})?;
	let Some(container_id) = current.container_id.as_deref() else {
		return Err(ServerError::Validation(
			"running deployment has no container recorded".to_string(),
		));
	};

	let server = scope::server(&s.db, team, app.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let path = format!(
		"/v1/containers/{container_id}/logs?tail={}&follow={follow}",
		tail.min(MAX_TAIL)
	);
	client.stream(Method::GET, &path, None::<&()>).await
}
