//! Replica use cases shared by the web UI: authorize, then scope, then validate, then touch the
//! database or the agent.

use axum::http::Method;
use proto::{ContainerHealth, ContainerInfo, Stats};
use sqlx::PgPool;

use super::status::{self, ReplicaRow, Rollup};
use super::{ReplicaConfig, config};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::deployments;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

/// What the form sends, as text: a number input comes back from the browser as a string.
#[derive(Debug, Clone, Default)]
pub struct ReplicaInput {
	pub replicas: String,
	pub sticky: bool,
	pub max_surge: String,
	pub max_unavailable: String,
	pub shared_volumes: bool,
}

fn number(raw: &str, default: i32, what: &str) -> Result<i32, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(default);
	}
	raw.parse::<i32>()
		.map_err(|_| ServerError::Validation(format!("{what} must be a whole number")))
}

impl ReplicaInput {
	/// The settings the form describes.
	///
	/// # Errors
	/// [`ServerError::Validation`] for a field that is not a number or is out of range.
	pub fn parse(&self) -> Result<ReplicaConfig, ServerError> {
		let d = ReplicaConfig::default();
		let config = ReplicaConfig {
			replicas: number(&self.replicas, d.replicas, "Replicas")?,
			sticky: self.sticky,
			max_surge: number(&self.max_surge, d.max_surge, "The surge")?,
			max_unavailable: number(
				&self.max_unavailable,
				d.max_unavailable,
				"The unavailable limit",
			)?,
			shared_volumes: self.shared_volumes,
		};
		config.validate()?;
		Ok(config)
	}
}

/// The settings with what the form needs to explain them.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
	pub config: ReplicaConfig,
	/// Volumes the application has; more than one replica needs them marked shared.
	pub volumes: usize,
}

/// The settings and the application's volume count; needs `read application`.
pub async fn settings(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Settings> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(db, team, project, app).await?;
	Ok(Settings {
		config: config(db, app.id).await?.unwrap_or_default(),
		volumes: crate::volumes::list(db, app.id).await?.len(),
	})
}

/// Stores the settings; needs `update application`. They apply from the next deploy.
///
/// # Errors
/// [`ServerError::Validation`] when the settings are out of range, or ask for more than one
/// replica of an application with volumes that were not marked shared.
pub async fn save(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	new: &ReplicaConfig,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(db, team, project, app).await?;
	new.validate()?;
	new.check_volumes(crate::volumes::list(db, app.id).await?.len())?;
	let before = config(db, app.id).await?;
	// An application that never had a setting and keeps the defaults stays without one: its
	// containers keep the labels they have, and its next deploy swaps them without a gap.
	if before.is_none() && *new == ReplicaConfig::default() {
		return Ok(());
	}
	let before = before.unwrap_or_default();
	super::save(db, app.id, new).await?;
	if before != *new {
		audit::log(
			db,
			user,
			team,
			"app.replicas",
			Target::new("application", app.id.0)
				.named(&app.slug)
				.in_project(project),
			Diff::new()
				.changed("replicas", before.replicas, new.replicas)
				.changed("sticky", before.sticky, new.sticky)
				.changed("max_surge", before.max_surge, new.max_surge)
				.changed(
					"max_unavailable",
					before.max_unavailable,
					new.max_unavailable,
				)
				.changed("shared_volumes", before.shared_volumes, new.shared_volumes),
		)
		.await;
	}
	Ok(())
}

/// The replicas of an application as they are now.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
	pub config: ReplicaConfig,
	pub rows: Vec<ReplicaRow>,
	pub rollup: Rollup,
}

/// Every replica of the running deployment with its state, restarts, CPU and memory; needs
/// `read application`. `None` when the application runs a single container (there is no
/// list to show). When the agent cannot be reached the rows are empty and the rollup says so.
pub async fn status(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Option<Live>> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let Some(config) = config(&s.db, app.id)
		.await?
		.filter(ReplicaConfig::is_scaled)
	else {
		return Ok(None);
	};
	let wanted = usize::try_from(config.replicas).unwrap_or(1);
	let rows = match deployments::latest_running(&s.db, app.id).await? {
		Some(deployment) => match fetch(s, team, &app, deployment.id.0).await {
			Ok(rows) => rows,
			Err(e) => {
				tracing::warn!(application = app.id.0, "cannot read the replicas: {e}");
				Vec::new()
			}
		},
		None => Vec::new(),
	};
	let rollup = status::rollup(wanted, &rows);
	Ok(Some(Live {
		config,
		rows,
		rollup,
	}))
}

async fn fetch(
	s: &AppState,
	team: TeamId,
	app: &crate::applications::Application,
	deployment: i64,
) -> Result<Vec<ReplicaRow>, ServerError> {
	let server = scope::server(&s.db, team, app.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let listed = names(&client, app.id.0, deployment).await?;
	let mut reports = Vec::with_capacity(listed.len());
	for container in &listed {
		let report: ContainerHealth = client
			.json(
				Method::GET,
				&format!("/v1/containers/{}/health", container.id),
				None::<&()>,
			)
			.await?;
		reports.push(report);
	}
	let stats: Stats = client.json(Method::GET, "/v1/stats", None::<&()>).await?;
	Ok(status::rows(
		app.id.0,
		deployment,
		&reports,
		&stats.containers,
	))
}

/// The agent's containers that are replicas of `deployment`.
pub async fn names(
	client: &crate::agents::AgentClient,
	app: i64,
	deployment: i64,
) -> Result<Vec<ContainerInfo>, ServerError> {
	let all: Vec<ContainerInfo> = client
		.json(
			Method::GET,
			&format!("/v1/containers?prefix=podkit-app-{app}-{deployment}"),
			None::<&()>,
		)
		.await?;
	Ok(all
		.into_iter()
		.filter(|c| status::number_of(&c.name, app, deployment).is_some())
		.collect())
}
