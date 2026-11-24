//! Builds: where an application's image is built, what a build may use, and the layer cache.
//!
//! - The team has a default **build server** (or none: an application builds on the server
//!   it runs on). An application may name a server of its own. The image is built there
//!   and moved to the server that runs it ([`delivery`]).
//! - A build container is capped in CPU and memory (team-wide), a build server runs at most
//!   so many builds at once ([`slots`]), and its layer cache is pruned to a size after each
//!   build.
//! - Everything is optional: with no rows an application builds on its own server, with the
//!   cache on and no caps, exactly as before.

pub mod delivery;
pub mod service;
pub mod slots;

#[cfg(test)]
mod tests;

use proto::BuildOptions;
use sqlx::{PgExecutor, PgPool};

use crate::ids::{ApplicationId, ServerId, TeamId};

/// Cap on the cache of a server that was never configured: 10 GB.
pub const DEFAULT_CACHE_MAX_MB: i32 = 10_240;
/// Builds a server runs at once when it was never configured.
pub const DEFAULT_MAX_CONCURRENT: i32 = 2;

/// The team's build defaults.
#[derive(Debug, Clone, Copy, PartialEq, Default, sqlx::FromRow)]
pub struct TeamSettings {
	/// The server that builds for the team's applications; `None`: each builds where it runs.
	pub build_server_id: Option<ServerId>,
	/// Cores a build may use.
	pub cpu_limit: Option<f32>,
	/// Megabytes a build may use.
	pub memory_limit_mb: Option<i32>,
}

/// How a server behaves as a builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerSettings {
	/// Builds that run at once; the rest wait their turn.
	pub max_concurrent: i32,
	/// The layer cache is pruned to this many megabytes after a build.
	pub cache_max_mb: i32,
}

impl Default for ServerSettings {
	fn default() -> Self {
		Self {
			max_concurrent: DEFAULT_MAX_CONCURRENT,
			cache_max_mb: DEFAULT_CACHE_MAX_MB,
		}
	}
}

/// One application's build choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSettings {
	/// A server of its own to build on; `None` follows the team.
	pub build_server_id: Option<ServerId>,
	/// Whether a build reuses the layers of earlier ones.
	pub cache_enabled: bool,
}

impl Default for AppSettings {
	fn default() -> Self {
		Self {
			build_server_id: None,
			cache_enabled: true,
		}
	}
}

/// The team's defaults; the default when nothing was ever saved.
pub async fn team_settings(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<TeamSettings, sqlx::Error> {
	Ok(sqlx::query_as!(
		TeamSettings,
		r#"
			SELECT build_server_id AS "build_server_id: ServerId", cpu_limit, memory_limit_mb
			FROM build_settings WHERE team_id = $1
		"#,
		team as TeamId
	)
	.fetch_optional(db)
	.await?
	.unwrap_or_default())
}

pub async fn save_team_settings(
	db: impl PgExecutor<'_>,
	team: TeamId,
	settings: &TeamSettings,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO build_settings (team_id, build_server_id, cpu_limit, memory_limit_mb)
			VALUES ($1, $2, $3, $4)
			ON CONFLICT (team_id) DO UPDATE SET
				build_server_id = EXCLUDED.build_server_id,
				cpu_limit = EXCLUDED.cpu_limit,
				memory_limit_mb = EXCLUDED.memory_limit_mb
		"#,
		team as TeamId,
		settings.build_server_id as Option<ServerId>,
		settings.cpu_limit,
		settings.memory_limit_mb,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// A server's builder settings; the defaults when nothing was ever saved.
pub async fn server_settings(
	db: impl PgExecutor<'_>,
	server: ServerId,
) -> Result<ServerSettings, sqlx::Error> {
	Ok(sqlx::query!(
		"SELECT max_concurrent, cache_max_mb FROM server_build_settings WHERE server_id = $1",
		server as ServerId
	)
	.fetch_optional(db)
	.await?
	.map_or_else(ServerSettings::default, |r| ServerSettings {
		max_concurrent: r.max_concurrent,
		cache_max_mb: r.cache_max_mb,
	}))
}

pub async fn save_server_settings(
	db: impl PgExecutor<'_>,
	server: ServerId,
	settings: &ServerSettings,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO server_build_settings (server_id, max_concurrent, cache_max_mb)
			VALUES ($1, $2, $3)
			ON CONFLICT (server_id) DO UPDATE SET
				max_concurrent = EXCLUDED.max_concurrent,
				cache_max_mb = EXCLUDED.cache_max_mb
		"#,
		server as ServerId,
		settings.max_concurrent,
		settings.cache_max_mb,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// An application's build choices; the defaults when nothing was ever saved.
pub async fn app_settings(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<AppSettings, sqlx::Error> {
	Ok(sqlx::query!(
		r#"SELECT build_server_id AS "build_server_id: ServerId", cache_enabled
			FROM app_build_settings WHERE application_id = $1"#,
		app as ApplicationId
	)
	.fetch_optional(db)
	.await?
	.map_or_else(AppSettings::default, |r| AppSettings {
		build_server_id: r.build_server_id,
		cache_enabled: r.cache_enabled,
	}))
}

pub async fn save_app_settings(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	settings: &AppSettings,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO app_build_settings (application_id, build_server_id, cache_enabled)
			VALUES ($1, $2, $3)
			ON CONFLICT (application_id) DO UPDATE SET
				build_server_id = EXCLUDED.build_server_id,
				cache_enabled = EXCLUDED.cache_enabled
		"#,
		app as ApplicationId,
		settings.build_server_id as Option<ServerId>,
		settings.cache_enabled,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// The server that builds for an application: its own choice, else the team's, else the
/// server it runs on.
#[must_use]
pub fn choose_server(runs_on: ServerId, app: &AppSettings, team: &TeamSettings) -> ServerId {
	app.build_server_id
		.or(team.build_server_id)
		.unwrap_or(runs_on)
}

/// Everything one build needs to know, decided before it starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
	/// The server that builds.
	pub server: ServerId,
	/// Whether that is another server than the one the application runs on, so the image
	/// has to be moved.
	pub remote: bool,
	/// Cache, caps and the cache budget, as the agent gets them.
	pub cache: bool,
	pub cpu_cores: Option<f64>,
	pub memory_bytes: Option<i64>,
	pub cache_max_bytes: u64,
	/// Builds the build server runs at once.
	pub max_concurrent: usize,
}

impl Plan {
	/// What the agent is told.
	#[must_use]
	pub fn options(&self) -> BuildOptions {
		BuildOptions {
			cache: self.cache,
			cpu_cores: self.cpu_cores,
			memory_bytes: self.memory_bytes,
			cache_max_bytes: Some(self.cache_max_bytes),
		}
	}
}

/// Puts the choices together: which server, the caps and the cache budget.
#[must_use]
pub fn plan(
	runs_on: ServerId,
	app: &AppSettings,
	team: &TeamSettings,
	builder: ServerSettings,
) -> Plan {
	let server = choose_server(runs_on, app, team);
	Plan {
		server,
		remote: server != runs_on,
		cache: app.cache_enabled,
		cpu_cores: team.cpu_limit.map(f64::from),
		memory_bytes: team.memory_limit_mb.map(|mb| i64::from(mb) * 1024 * 1024),
		cache_max_bytes: u64::try_from(builder.cache_max_mb.max(0)).unwrap_or(0) * 1024 * 1024,
		max_concurrent: usize::try_from(builder.max_concurrent.max(1)).unwrap_or(1),
	}
}

/// The plan for an application, read from the database. A build server that is being
/// drained takes no new builds: the application then builds where it runs.
pub async fn plan_for(
	db: &PgPool,
	team: TeamId,
	app: ApplicationId,
	runs_on: ServerId,
) -> Result<Plan, sqlx::Error> {
	let mut team_settings = team_settings(db, team).await?;
	let mut app_settings = app_settings(db, app).await?;
	let mut server = choose_server(runs_on, &app_settings, &team_settings);
	if server != runs_on && crate::moves::draining_since(db, server).await?.is_some() {
		app_settings.build_server_id = None;
		team_settings.build_server_id = None;
		server = runs_on;
	}
	let builder = server_settings(db, server).await?;
	Ok(plan(runs_on, &app_settings, &team_settings, builder))
}

/// What the agent is told for a build that runs on the application's own server (a deploy
/// that builds). A server that was never configured gets the defaults.
pub async fn options_for(
	db: &PgPool,
	application: &crate::applications::Application,
) -> Result<BuildOptions, sqlx::Error> {
	let team = sqlx::query_scalar!(
		r#"SELECT team_id AS "team_id!: TeamId" FROM projects WHERE id = $1"#,
		application.project_id.0,
	)
	.fetch_one(db)
	.await?;
	Ok(plan_for(db, team, application.id, application.server_id)
		.await?
		.options())
}
