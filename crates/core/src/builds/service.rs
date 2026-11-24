//! Build use cases for the web UI: authorize, then scope, then validate, then touch the
//! database or the agent. A server or application of another team behaves like a missing one.

use std::time::Duration;

use proto::BuildCachePruned;

use super::{AppSettings, ServerSettings, TeamSettings};
use crate::applications::BuildStrategy;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId, UserId};
use crate::servers::{Server, ServerStatus};
use crate::state::AppState;
use crate::{scope, servers, validate};

/// A server that does not answer within this has no cache size on the page, not a slow page.
const CACHE_PROBE: Duration = Duration::from_secs(3);

/// The most cores or megabytes a form may ask for; a typo should not become a cap that never
/// applies.
const MAX_CPU: f32 = 256.0;
const MAX_MEMORY_MB: i32 = 1_048_576;
const MAX_CACHE_MB: i32 = 10_000_000;

/// One server on the Builds page.
#[derive(Debug, Clone)]
pub struct ServerRow {
	pub server: Server,
	pub settings: ServerSettings,
	/// What the layer cache takes now; `None` when the server did not say.
	pub cache: Option<proto::BuildCacheInfo>,
	/// Builds running on it now.
	pub running: usize,
}

/// What Settings → Builds shows.
#[derive(Debug, Clone)]
pub struct TeamView {
	pub settings: TeamSettings,
	pub servers: Vec<ServerRow>,
}

/// What the team form sends, as text.
#[derive(Debug, Clone, Default)]
pub struct TeamInput {
	/// A server's id, or empty for none.
	pub build_server: String,
	/// Cores, or empty for no cap.
	pub cpu: String,
	/// Megabytes, or empty for no cap.
	pub memory_mb: String,
}

/// What a server's builder form sends, as text.
#[derive(Debug, Clone, Default)]
pub struct ServerInput {
	pub max_concurrent: String,
	pub cache_max_mb: String,
}

/// What the application's Build tab shows.
#[derive(Debug, Clone)]
pub struct AppView {
	pub settings: AppSettings,
	pub strategy: BuildStrategy,
	pub dockerfile_path: String,
	/// Whether the application is built from a repository at all.
	pub git: bool,
	/// The team's default builder, so the page can say what "follow the team" means.
	pub team_server: Option<ServerId>,
	pub servers: Vec<Server>,
}

/// What the Build tab form sends, as text.
#[derive(Debug, Clone, Default)]
pub struct AppInput {
	/// A server's id, or empty to follow the team.
	pub build_server: String,
	pub cache: bool,
	/// `dockerfile` or `nixpacks`; empty keeps the stored strategy.
	pub strategy: String,
	pub dockerfile_path: String,
}

fn parse_optional<T: std::str::FromStr>(raw: &str, what: &str) -> AppResult<Option<T>> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{what} must be a number.")))
}

fn parse_required<T: std::str::FromStr>(raw: &str, what: &str) -> AppResult<T> {
	parse_optional(raw, what)?
		.ok_or_else(|| ServerError::Validation(format!("{what} is required.")))
}

impl TeamInput {
	/// The settings this describes. The server is only an id here; [`save_team`] checks it
	/// belongs to the team.
	///
	/// # Errors
	/// [`ServerError::Validation`] for a field that is not a number or is out of range.
	pub fn parse(&self) -> AppResult<TeamSettings> {
		let build_server_id =
			parse_optional::<i64>(&self.build_server, "The build server")?.map(ServerId);
		let cpu_limit = parse_optional::<f32>(&self.cpu, "The CPU cap")?;
		if let Some(cpu) = cpu_limit
			&& !(cpu > 0.0 && cpu <= MAX_CPU)
		{
			return Err(ServerError::Validation(format!(
				"The CPU cap must be more than 0 and at most {MAX_CPU} cores."
			)));
		}
		let memory_limit_mb = parse_optional::<i32>(&self.memory_mb, "The memory cap")?;
		if let Some(mb) = memory_limit_mb
			&& !(256..=MAX_MEMORY_MB).contains(&mb)
		{
			return Err(ServerError::Validation(
				"The memory cap must be at least 256 MB.".to_owned(),
			));
		}
		Ok(TeamSettings {
			build_server_id,
			cpu_limit,
			memory_limit_mb,
		})
	}
}

impl ServerInput {
	/// # Errors
	/// [`ServerError::Validation`] for a field that is not a number or is out of range.
	pub fn parse(&self) -> AppResult<ServerSettings> {
		let max_concurrent: i32 = parse_required(&self.max_concurrent, "Builds at once")?;
		if !(1..=16).contains(&max_concurrent) {
			return Err(ServerError::Validation(
				"Builds at once must be between 1 and 16.".to_owned(),
			));
		}
		let cache_max_mb: i32 = parse_required(&self.cache_max_mb, "The cache size")?;
		if !(0..=MAX_CACHE_MB).contains(&cache_max_mb) {
			return Err(ServerError::Validation(
				"The cache size must be 0 or more megabytes.".to_owned(),
			));
		}
		Ok(ServerSettings {
			max_concurrent,
			cache_max_mb,
		})
	}
}

/// The server a form named, if it is one of this team's and can build.
async fn builder(s: &AppState, team: TeamId, id: ServerId) -> AppResult<Server> {
	let server = scope::server(&s.db, team, id).await?;
	if server.status != ServerStatus::Active {
		return Err(ServerError::Validation(format!(
			"{} is not ready: its agent is not running.",
			server.name
		)));
	}
	Ok(server)
}

/// The Builds page: the team's defaults and every server with its cache; needs `read server`.
pub async fn team_view(s: &AppState, user: UserId, team: TeamId) -> AppResult<TeamView> {
	let all = servers::service::list(&s.db, user, team).await?;
	let settings = super::team_settings(&s.db, team).await?;
	let mut rows = Vec::with_capacity(all.len());
	for server in all {
		let server_settings = super::server_settings(&s.db, server.id).await?;
		rows.push((server, server_settings));
	}
	// Ask every server at once; a slow one costs the timeout, not a sum of them.
	let probes = rows.iter().map(|(server, _)| cache_size(s, server));
	let caches = futures_util::future::join_all(probes).await;
	let servers = rows
		.into_iter()
		.zip(caches)
		.map(|((server, settings), cache)| ServerRow {
			running: super::slots::running(server.id),
			server,
			settings,
			cache,
		})
		.collect();
	Ok(TeamView { settings, servers })
}

async fn cache_size(s: &AppState, server: &Server) -> Option<proto::BuildCacheInfo> {
	if server.status != ServerStatus::Active {
		return None;
	}
	let probe = async {
		let client = s.agents.client(s, server).await.ok()?;
		client.build_cache().await.ok()
	};
	tokio::time::timeout(CACHE_PROBE, probe)
		.await
		.ok()
		.flatten()
}

/// Saves the team's build server and the caps of a build; needs `update server`.
pub async fn save_team(
	s: &AppState,
	user: UserId,
	team: TeamId,
	input: &TeamInput,
) -> AppResult<TeamSettings> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let settings = input.parse()?;
	if let Some(id) = settings.build_server_id {
		builder(s, team, id).await?;
	}
	let before = super::team_settings(&s.db, team).await?;
	super::save_team_settings(&s.db, team, &settings).await?;
	audit::log(
		&s.db,
		user,
		team,
		"builds.settings",
		Target::new("team", team.0),
		Diff::new()
			.changed(
				"build_server",
				before.build_server_id.map(|i| i.0),
				settings.build_server_id.map(|i| i.0),
			)
			.changed("cpu_limit", before.cpu_limit, settings.cpu_limit)
			.changed(
				"memory_limit_mb",
				before.memory_limit_mb,
				settings.memory_limit_mb,
			),
	)
	.await;
	Ok(settings)
}

/// The whole Builds form: the team's settings and every listed server's. Everything is
/// validated before anything is stored, so a bad number cannot leave the page half saved;
/// needs `update server`.
pub async fn save_all(
	s: &AppState,
	user: UserId,
	team: TeamId,
	team_input: &TeamInput,
	servers: &[(ServerId, ServerInput)],
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	team_input.parse()?;
	for (_, input) in servers {
		input.parse()?;
	}
	for (id, _) in servers {
		scope::server(&s.db, team, *id).await?;
	}
	save_team(s, user, team, team_input).await?;
	for (id, input) in servers {
		save_server(s, user, team, *id, input).await?;
	}
	Ok(())
}

/// Saves how a server builds; needs `update server`.
pub async fn save_server(
	s: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	input: &ServerInput,
) -> AppResult<ServerSettings> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&s.db, team, server).await?;
	let settings = input.parse()?;
	let before = super::server_settings(&s.db, server.id).await?;
	super::save_server_settings(&s.db, server.id, &settings).await?;
	audit::log(
		&s.db,
		user,
		team,
		"builds.server",
		Target::new("server", server.id.0).named(&server.name),
		Diff::new()
			.changed(
				"max_concurrent",
				before.max_concurrent,
				settings.max_concurrent,
			)
			.changed("cache_max_mb", before.cache_max_mb, settings.cache_max_mb),
	)
	.await;
	Ok(settings)
}

/// How far to prune.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneTo {
	/// Down to the server's cache size.
	Limit,
	/// Everything that is not in use.
	Empty,
}

/// Prunes a server's layer cache now; needs `update server`.
pub async fn prune_cache(
	s: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	to: PruneTo,
) -> AppResult<BuildCachePruned> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&s.db, team, server).await?;
	let max_bytes = match to {
		PruneTo::Empty => 0,
		PruneTo::Limit => {
			let settings = super::server_settings(&s.db, server.id).await?;
			u64::try_from(settings.cache_max_mb.max(0)).unwrap_or(0) * 1024 * 1024
		}
	};
	let client = s.agents.client(s, &server).await?;
	let pruned = client.prune_build_cache(max_bytes).await?;
	audit::log(
		&s.db,
		user,
		team,
		"builds.prune",
		Target::new("server", server.id.0).named(&server.name),
		Diff::new()
			.set("layers_removed", i64::from(pruned.removed))
			.set(
				"freed_bytes",
				i64::try_from(pruned.freed_bytes).unwrap_or(i64::MAX),
			),
	)
	.await;
	Ok(pruned)
}

/// The Build tab: needs `read application`. An application of another team, or of another
/// project, is `ApplicationNotFound`.
pub async fn app_view(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<AppView> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let application = scope::application(&s.db, team, project, app).await?;
	let settings = super::app_settings(&s.db, application.id).await?;
	let team_settings = super::team_settings(&s.db, team).await?;
	let servers = servers::list_by_team(&s.db, team).await?;
	Ok(AppView {
		settings,
		strategy: application.build_strategy,
		dockerfile_path: application.dockerfile_path,
		git: application.source_kind == crate::applications::SourceKind::Git,
		team_server: team_settings.build_server_id,
		servers,
	})
}

/// Saves the Build tab: the strategy, whether builds reuse layers, and the server it builds
/// on; needs `update application`. Applies from the next deploy.
pub async fn save_app(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	input: &AppInput,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let application = scope::application(&s.db, team, project, app).await?;
	let build_server_id =
		parse_optional::<i64>(&input.build_server, "The build server")?.map(ServerId);
	if let Some(id) = build_server_id {
		builder(s, team, id).await?;
	}
	let strategy = match input.strategy.as_str() {
		"" => application.build_strategy,
		"dockerfile" => BuildStrategy::Dockerfile,
		"nixpacks" => BuildStrategy::Nixpacks,
		_ => {
			return Err(ServerError::Validation(
				"The build strategy must be dockerfile or nixpacks.".to_owned(),
			));
		}
	};
	let dockerfile_path = if input.dockerfile_path.trim().is_empty() {
		application.dockerfile_path.clone()
	} else {
		input.dockerfile_path.trim().to_owned()
	};
	if !validate::rel_path(&dockerfile_path) {
		return Err(ServerError::Validation(
			"The Dockerfile path must be relative to the repository, without \"..\".".to_owned(),
		));
	}
	let before = super::app_settings(&s.db, application.id).await?;
	let settings = AppSettings {
		build_server_id,
		cache_enabled: input.cache,
	};
	let mut tx = s.db.begin().await?;
	super::save_app_settings(&mut *tx, application.id, &settings).await?;
	sqlx::query!(
		"UPDATE applications SET build_strategy = $2, dockerfile_path = $3 WHERE id = $1",
		application.id as ApplicationId,
		strategy.as_str(),
		dockerfile_path,
	)
	.execute(&mut *tx)
	.await?;
	tx.commit().await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.build",
		Target::new("application", application.id.0)
			.named(&application.slug)
			.in_project(project),
		Diff::new()
			.changed(
				"build_server",
				before.build_server_id.map(|i| i.0),
				settings.build_server_id.map(|i| i.0),
			)
			.changed("cache", before.cache_enabled, settings.cache_enabled)
			.changed(
				"strategy",
				application.build_strategy.as_str(),
				strategy.as_str(),
			)
			.changed(
				"dockerfile_path",
				application.dockerfile_path.as_str(),
				dockerfile_path.as_str(),
			),
	)
	.await;
	Ok(())
}
