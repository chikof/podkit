//! Health use cases shared by the web UI: authorize, then scope, then validate, then touch the
//! database.

use sqlx::PgPool;

use super::{HealthConfig, Kind};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::scope;

/// What the health form sends, as text: a number input comes back from the browser as a string.
#[derive(Debug, Clone, Default)]
pub struct HealthInput {
	/// Whether the health gate is on at all.
	pub enabled: bool,
	/// `http` or `command`.
	pub kind: String,
	pub path: String,
	pub expect_status: String,
	pub command: String,
	pub interval_s: String,
	pub timeout_s: String,
	pub retries: String,
	pub start_period_s: String,
	pub gate_timeout_s: String,
	pub drain_s: String,
	pub rollback_window_min: String,
	pub rollback_after: String,
}

fn number(raw: &str, default: i32, what: &str) -> Result<i32, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(default);
	}
	raw.parse::<i32>()
		.map_err(|_| ServerError::Validation(format!("{what} must be a whole number")))
}

impl HealthInput {
	/// The settings the form describes, or `None` when the gate is off.
	///
	/// # Errors
	/// [`ServerError::Validation`] for a field that is not a number or is out of range.
	pub fn parse(&self) -> Result<Option<HealthConfig>, ServerError> {
		if !self.enabled {
			return Ok(None);
		}
		let d = HealthConfig::default();
		let kind = match self.kind.as_str() {
			"" | "http" => Kind::Http,
			"command" => Kind::Command,
			_ => {
				return Err(ServerError::Validation(
					"The health check type must be http or command".to_owned(),
				));
			}
		};
		let config = HealthConfig {
			kind,
			path: if self.path.trim().is_empty() {
				d.path
			} else {
				self.path.trim().to_owned()
			},
			expect_status: number(&self.expect_status, d.expect_status, "The expected status")?,
			command: Some(self.command.trim().to_owned()).filter(|c| !c.is_empty()),
			interval_s: number(&self.interval_s, d.interval_s, "The interval")?,
			timeout_s: number(&self.timeout_s, d.timeout_s, "The timeout")?,
			retries: number(&self.retries, d.retries, "The retries")?,
			start_period_s: number(&self.start_period_s, d.start_period_s, "The start period")?,
			gate_timeout_s: number(&self.gate_timeout_s, d.gate_timeout_s, "The deploy wait")?,
			drain_s: number(&self.drain_s, d.drain_s, "The drain time")?,
			rollback_window_min: number(
				&self.rollback_window_min,
				d.rollback_window_min,
				"The rollback window",
			)?,
			rollback_after: number(
				&self.rollback_after,
				d.rollback_after,
				"The rollback threshold",
			)?,
		};
		config.validate()?;
		Ok(Some(config))
	}
}

/// The application's health settings; needs `read application`. `None` means the gate is off.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Option<HealthConfig>> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(db, team, project, app).await?;
	Ok(super::config(db, app.id).await?)
}

/// Stores the settings (or turns the gate off with `None`); needs `update application`.
/// They apply from the next deploy.
pub async fn save(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	config: Option<&HealthConfig>,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(db, team, project, app).await?;
	match config {
		Some(config) => {
			config.validate()?;
			super::save(db, app.id, config).await?;
		}
		None => super::remove(db, app.id).await?,
	}
	Ok(())
}
