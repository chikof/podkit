//! Health-gated deploys: what counts as healthy for an application, what the deploy does with
//! it, and the automatic rollback when a release goes bad shortly after it went live.
//!
//! - [`HealthConfig`] is the per-application setting (`application_health`); without a row
//!   an application deploys the way it always did.
//! - [`machine`] is the pure state machine of one deployment's health: waiting, healthy,
//!   unhealthy (the old container kept serving) or rolled back.
//! - [`service`] reads and saves the setting; [`watch`] is the periodic check that drives the
//!   machine after the deploy, and queues the rollback.

pub mod machine;
pub mod service;
pub mod watch;

#[cfg(test)]
mod tests;

use proto::{HealthCheck, HealthGate};
use sqlx::{PgExecutor, PgPool};

use crate::error::ServerError;
use crate::ids::{ApplicationId, DeploymentId};

/// How an application's health is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Kind {
	/// `GET path` on the container's port, wanting `expect_status`.
	Http,
	/// A command run in the container; exit 0 is healthy.
	Command,
}

/// The health settings of one application, in seconds and counts.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HealthConfig {
	pub kind: Kind,
	pub path: String,
	pub expect_status: i32,
	pub command: Option<String>,
	pub interval_s: i32,
	pub timeout_s: i32,
	pub retries: i32,
	pub start_period_s: i32,
	/// Seconds a deploy waits for the new container to be healthy.
	pub gate_timeout_s: i32,
	/// Seconds the old container keeps serving once the new one is healthy.
	pub drain_s: i32,
	/// Minutes after a deploy during which a bad release is rolled back; 0 is off.
	pub rollback_window_min: i32,
	/// Consecutive failed checks that count as a bad release.
	pub rollback_after: i32,
}

impl Default for HealthConfig {
	fn default() -> Self {
		Self {
			kind: Kind::Http,
			path: "/".to_owned(),
			expect_status: 200,
			command: None,
			interval_s: 5,
			timeout_s: 3,
			retries: 3,
			start_period_s: 10,
			gate_timeout_s: 90,
			drain_s: 5,
			rollback_window_min: 5,
			rollback_after: 3,
		}
	}
}

impl HealthConfig {
	/// The gate the agent runs. `port` is the application's container port.
	#[must_use]
	pub fn to_gate(&self, port: u16) -> HealthGate {
		let secs = |s: i32| u64::try_from(s).unwrap_or(0);
		let count = |n: i32| u32::try_from(n).unwrap_or(0);
		HealthGate {
			check: match self.kind {
				Kind::Http => HealthCheck::Http {
					path: self.path.clone(),
					status: u16::try_from(self.expect_status).unwrap_or(200),
					port,
				},
				Kind::Command => HealthCheck::Command {
					command: self.command.clone().unwrap_or_default(),
				},
			},
			interval_ms: secs(self.interval_s) * 1000,
			timeout_ms: secs(self.timeout_s) * 1000,
			retries: count(self.retries),
			start_period_ms: secs(self.start_period_s) * 1000,
			gate_timeout_s: count(self.gate_timeout_s),
			drain_s: count(self.drain_s),
		}
	}

	/// Whether a bad release is rolled back at all.
	#[must_use]
	pub fn rolls_back(&self) -> bool {
		self.rollback_window_min > 0
	}

	/// Checks every field against the limits the table enforces, with a message the form can
	/// show.
	///
	/// # Errors
	/// [`ServerError::Validation`] naming the field.
	pub fn validate(&self) -> Result<(), ServerError> {
		let bad = |message: &str| Err(ServerError::Validation(message.to_owned()));
		let range = |value: i32, low: i32, high: i32, what: &str| {
			if (low..=high).contains(&value) {
				Ok(())
			} else {
				Err(ServerError::Validation(format!(
					"{what} must be between {low} and {high}"
				)))
			}
		};
		match self.kind {
			Kind::Http => {
				if !self.path.starts_with('/')
					|| self.path.len() > 512
					|| self.path.chars().any(|c| c.is_control() || c == ' ')
				{
					return bad("The health check path must start with / and contain no spaces");
				}
				range(self.expect_status, 100, 599, "The expected status")?;
			}
			Kind::Command => match self.command.as_deref().map(str::trim) {
				None | Some("") => return bad("Enter the command the health check runs"),
				Some(c) if c.len() > 1024 || c.contains('\0') => {
					return bad("The health check command is too long");
				}
				Some(_) => {}
			},
		}
		range(self.interval_s, 1, 300, "The interval")?;
		range(self.timeout_s, 1, 120, "The timeout")?;
		range(self.retries, 1, 20, "The retries")?;
		range(self.start_period_s, 0, 600, "The start period")?;
		range(self.gate_timeout_s, 5, 1800, "The deploy wait")?;
		range(self.drain_s, 0, 300, "The drain time")?;
		range(self.rollback_window_min, 0, 120, "The rollback window")?;
		range(self.rollback_after, 1, 20, "The rollback threshold")?;
		Ok(())
	}
}

/// Where a deployment stands with the health gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum State {
	/// The new container runs and has not passed its check yet.
	Waiting,
	/// It passed and took over.
	Healthy,
	/// It never passed; the previous container kept serving and the deploy failed.
	Unhealthy,
	/// It went live and then failed; the previous image was put back.
	RolledBack,
}

/// What the gate recorded about one deployment.
#[derive(Debug, Clone, sqlx::FromRow)]
#[cfg(test)]
pub struct DeploymentHealth {
	pub state: State,
	pub detail: Option<String>,
	/// Set on a deployment an automatic rollback started: the one it replaced.
	pub rollback_of: Option<i64>,
}

/// The application's health settings, if it has any.
pub async fn config(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Option<HealthConfig>, sqlx::Error> {
	sqlx::query_as::<_, HealthConfig>(
		"SELECT kind, path, expect_status, command, interval_s, timeout_s, retries, start_period_s, \
		 gate_timeout_s, drain_s, rollback_window_min, rollback_after \
		 FROM application_health WHERE application_id = $1",
	)
	.bind(app.0)
	.fetch_optional(db)
	.await
}

/// Stores the application's health settings.
pub async fn save(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	c: &HealthConfig,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO application_health (application_id, kind, path, expect_status, command, interval_s, \
		 timeout_s, retries, start_period_s, gate_timeout_s, drain_s, rollback_window_min, rollback_after) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
		 ON CONFLICT (application_id) DO UPDATE SET kind = $2, path = $3, expect_status = $4, command = $5, \
		 interval_s = $6, timeout_s = $7, retries = $8, start_period_s = $9, gate_timeout_s = $10, \
		 drain_s = $11, rollback_window_min = $12, rollback_after = $13, updated_at = NOW()",
	)
	.bind(app.0)
	.bind(c.kind)
	.bind(&c.path)
	.bind(c.expect_status)
	.bind(&c.command)
	.bind(c.interval_s)
	.bind(c.timeout_s)
	.bind(c.retries)
	.bind(c.start_period_s)
	.bind(c.gate_timeout_s)
	.bind(c.drain_s)
	.bind(c.rollback_window_min)
	.bind(c.rollback_after)
	.execute(db)
	.await?;
	Ok(())
}

/// Turns the health gate off for the application.
pub async fn remove(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM application_health WHERE application_id = $1")
		.bind(app.0)
		.execute(db)
		.await?;
	Ok(())
}

/// Records where a deployment stands, keeping what an earlier write set (`rollback_of`).
pub async fn set_state(
	db: impl PgExecutor<'_>,
	deployment: DeploymentId,
	state: State,
	detail: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO deployment_health (deployment_id, state, detail) VALUES ($1, $2, $3) \
		 ON CONFLICT (deployment_id) DO UPDATE SET state = $2, detail = $3, updated_at = NOW()",
	)
	.bind(deployment.0)
	.bind(state)
	.bind(detail)
	.execute(db)
	.await?;
	Ok(())
}

/// Stores the failed-check count the watch has seen.
pub async fn set_failures(
	db: impl PgExecutor<'_>,
	deployment: DeploymentId,
	failures: i32,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE deployment_health SET failures = $2, updated_at = NOW() WHERE deployment_id = $1",
	)
	.bind(deployment.0)
	.bind(failures)
	.execute(db)
	.await?;
	Ok(())
}

/// Starts the row of a rollback deployment, naming what it replaces. A rollback runs a
/// known-good image, so the row starts out healthy; the deploy's own gate moves it from there.
pub async fn start_rollback(
	db: impl PgExecutor<'_>,
	deployment: DeploymentId,
	replaces: DeploymentId,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO deployment_health (deployment_id, state, rollback_of) VALUES ($1, 'healthy', $2) \
		 ON CONFLICT (deployment_id) DO UPDATE SET rollback_of = $2",
	)
	.bind(deployment.0)
	.bind(replaces.0)
	.execute(db)
	.await?;
	Ok(())
}

/// The gate's verdict and its detail for each of `deployments` that has one.
pub async fn verdicts(
	db: impl PgExecutor<'_>,
	deployments: &[DeploymentId],
) -> Result<std::collections::HashMap<DeploymentId, (State, Option<String>)>, sqlx::Error> {
	let ids: Vec<i64> = deployments.iter().map(|d| d.0).collect();
	let rows: Vec<(i64, State, Option<String>)> = sqlx::query_as(
		"SELECT deployment_id, state, detail FROM deployment_health WHERE deployment_id = ANY($1)",
	)
	.bind(ids)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|(id, state, detail)| (DeploymentId(id), (state, detail)))
		.collect())
}

/// A deploy that ended without a verdict must not stay "waiting for health": its row goes, or
/// for a rollback (whose row carries what it replaced) becomes unhealthy with `detail`.
pub async fn settle_waiting(
	db: &PgPool,
	deployment: DeploymentId,
	detail: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"DELETE FROM deployment_health WHERE deployment_id = $1 AND state = 'waiting' AND rollback_of IS NULL",
	)
	.bind(deployment.0)
	.execute(db)
	.await?;
	sqlx::query(
		"UPDATE deployment_health SET state = 'unhealthy', \
		 detail = COALESCE($2, 'the deploy ended before the check finished'), updated_at = NOW() \
		 WHERE deployment_id = $1 AND state = 'waiting'",
	)
	.bind(deployment.0)
	.bind(detail)
	.execute(db)
	.await?;
	Ok(())
}

/// What the gate recorded about one deployment.
#[cfg(test)]
pub async fn of_deployment(
	db: impl PgExecutor<'_>,
	deployment: DeploymentId,
) -> Result<Option<DeploymentHealth>, sqlx::Error> {
	sqlx::query_as::<_, DeploymentHealth>(
		"SELECT state, detail, rollback_of FROM deployment_health WHERE deployment_id = $1",
	)
	.bind(deployment.0)
	.fetch_optional(db)
	.await
}
