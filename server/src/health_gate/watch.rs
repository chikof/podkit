//! The watch after a deploy: for a few minutes after a release took over, look at its
//! container on every status sync and, when it crash-loops or keeps failing its health check,
//! queue a redeploy of the previous image.

use std::time::Duration;

use axum::http::Method;
use proto::{ContainerHealth, ContainerState, HealthStatus};
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::warn;

use super::State as Health;
use super::machine::{Effect, Machine, Signal};
use crate::agents::AgentClient;
use crate::deployments;
use crate::error::ServerError;
use crate::ids::{ApplicationId, DeploymentId, ServerId};
use crate::state::AppState;

#[derive(sqlx::FromRow)]
pub(super) struct Candidate {
	pub(super) id: DeploymentId,
	pub(super) application_id: ApplicationId,
	pub(super) container_id: String,
	pub(super) finished_at: OffsetDateTime,
	pub(super) failures: i32,
	pub(super) rollback_after: i32,
	pub(super) rollback_window_min: i32,
}

/// What one look at a container means for its release.
#[must_use]
pub fn signal(report: &ContainerHealth) -> Signal {
	if report.gave_up {
		return Signal::Crashed;
	}
	match (report.state, report.health) {
		(ContainerState::Exited | ContainerState::Dead, _) | (_, Some(HealthStatus::Unhealthy)) => {
			Signal::CheckFailed
		}
		_ => Signal::CheckPassed,
	}
}

/// Looks at every release on `server` that is still inside its rollback window, and rolls back
/// the ones that went bad. A failure to look is logged and leaves the release alone.
pub async fn sweep(state: &AppState, server: ServerId, client: &AgentClient) {
	if let Err(e) = look(state, server, client).await {
		warn!(server = %server, error = %e, "health watch failed");
	}
}

async fn look(state: &AppState, server: ServerId, client: &AgentClient) -> Result<(), ServerError> {
	for c in candidates(&state.db, server).await? {
		let path = format!("/v1/containers/{}/health", c.container_id);
		let Ok(report) = client
			.json::<ContainerHealth>(Method::GET, &path, None::<&()>)
			.await
		else {
			// The container is gone or the agent hiccuped; the crash sweep handles the former.
			continue;
		};
		apply(state, &c, signal(&report)).await?;
	}
	Ok(())
}

/// The releases on `server` that are live, passed their gate, and still inside their rollback
/// window.
pub(super) async fn candidates(
	db: &PgPool,
	server: ServerId,
) -> Result<Vec<Candidate>, sqlx::Error> {
	sqlx::query_as::<_, Candidate>(
		"SELECT d.id, d.application_id, d.container_id, d.finished_at, h.failures, \
		 ah.rollback_after, ah.rollback_window_min \
		 FROM deployments d \
		 JOIN applications a ON a.id = d.application_id \
		 JOIN application_health ah ON ah.application_id = a.id \
		 JOIN deployment_health h ON h.deployment_id = d.id \
		 WHERE a.server_id = $1 AND d.status = 'running' AND d.container_id IS NOT NULL \
		 AND d.finished_at IS NOT NULL AND h.state = 'healthy' AND h.rollback_of IS NULL \
		 AND ah.rollback_window_min > 0 \
		 AND d.finished_at > NOW() - make_interval(mins => ah.rollback_window_min)",
	)
	.bind(server.0)
	.fetch_all(db)
	.await
}

/// Feeds one look at a release to its machine, stores the failures it counted, and queues the
/// rollback when the machine says so.
pub(super) async fn apply(
	state: &AppState,
	c: &Candidate,
	signal: Signal,
) -> Result<(), ServerError> {
	let previous = previous_good(&state.db, c.application_id, c.id).await?;
	let since =
		Duration::try_from(OffsetDateTime::now_utc() - c.finished_at).unwrap_or(Duration::ZERO);
	let mut machine = Machine::new(
		c.rollback_after,
		Duration::from_secs(u64::try_from(c.rollback_window_min).unwrap_or(0) * 60),
		false,
		previous.is_some(),
	);
	machine.state = Some(Health::Healthy);
	machine.failures = c.failures;
	let effect = machine.on(signal, since);
	if machine.failures != c.failures {
		super::set_failures(&state.db, c.id, machine.failures).await?;
	}
	if let (Effect::Rollback(reason), Some(previous)) = (effect, previous) {
		roll_back(state, c, previous, &reason).await?;
	}
	Ok(())
}

/// The deployment to go back to: the newest earlier one that ran, kept its image, and was not
/// itself found bad.
pub async fn previous_good(
	db: &PgPool,
	app: ApplicationId,
	current: DeploymentId,
) -> Result<Option<DeploymentId>, sqlx::Error> {
	let id: Option<i64> = sqlx::query_scalar(
		"SELECT d.id FROM deployments d \
		 LEFT JOIN deployment_health h ON h.deployment_id = d.id \
		 WHERE d.application_id = $1 AND d.id <> $2 \
		 AND d.image_tag IS NOT NULL AND d.container_id IS NOT NULL \
		 AND d.status = 'stopped' \
		 AND d.created_at < (SELECT created_at FROM deployments WHERE id = $2) \
		 AND COALESCE(h.state, 'healthy') NOT IN ('unhealthy', 'rolled_back') \
		 ORDER BY d.created_at DESC LIMIT 1",
	)
	.bind(app.0)
	.bind(current.0)
	.fetch_optional(db)
	.await?;
	Ok(id.map(DeploymentId))
}

async fn roll_back(
	state: &AppState,
	bad: &Candidate,
	previous: DeploymentId,
	reason: &str,
) -> Result<(), ServerError> {
	super::set_state(&state.db, bad.id, Health::RolledBack, Some(reason)).await?;
	let Some(app) = crate::applications::find_by_id(&state.db, bad.application_id).await? else {
		return Ok(());
	};
	let Some(target) = deployments::find(&state.db, app.id, previous).await? else {
		return Ok(());
	};
	let started = deployments::queue(state, &app, None, Some(&target)).await?;
	super::start_rollback(&state.db, started.id, bad.id).await?;
	notify_rolled_back(&app.name, bad.id, started.id, reason);
	Ok(())
}

/// Tells the team a release was rolled back. The notification channels (roadmap A2) are not
/// merged here, so for now it is a warning in the server log; the timeline and the header
/// already show it.
// TODO(A2): emit a `deploy.rolled_back` notification event here.
fn notify_rolled_back(app: &str, bad: DeploymentId, started: DeploymentId, reason: &str) {
	warn!(
		application = app,
		deployment = bad.0,
		rollback = started.0,
		"rolled back automatically: {reason}"
	);
}
