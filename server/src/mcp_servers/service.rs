//! What the MCP server routes do, free of HTTP: authorize, scope to the team, validate,
//! then store and tell the agent.

use axum::http::Method;
use futures_util::StreamExt;
use proto::{Event, GatewayMcpToolsRequest};
use serde_json::Value;
use sqlx::PgPool;

use super::{Link, McpServer, Transport};
use crate::applications::service::{AppInput, ApplicationView};
use crate::applications::{self, Application};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::change::Change;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::{deployments, links, mcp_servers, scope, servers};

/// Port a new MCP server listens on unless told otherwise.
pub const DEFAULT_PORT: i32 = 8080;
/// Path of the streamable HTTP endpoint unless told otherwise.
pub const DEFAULT_PATH: &str = "/mcp";
const MAX_ARGS: usize = 64;
const MAX_ARG_LEN: usize = 1000;
/// Seconds each request of the tools probe may take.
const PROBE_TIMEOUT_S: u32 = 30;
const MAX_TOOLS: usize = 500;
const MAX_DESCRIPTION: usize = 400;

/// How the server is reached: the part of the create form that is about MCP.
#[derive(Debug, Clone)]
pub struct Settings {
	pub transport: Transport,
	pub path: Option<String>,
	/// stdio only; empty runs the image's own command.
	pub command: Vec<String>,
}

/// A validated [`Settings`], as stored.
struct Checked {
	path: String,
	command: Option<String>,
}

/// Whether `settings` can be stored.
///
/// # Errors
/// `Validation` naming the problem.
pub fn check_settings(settings: &Settings) -> AppResult<()> {
	check(settings).map(|_| ())
}

fn check(settings: &Settings) -> AppResult<Checked> {
	let bad = |m: &str| Err(ServerError::Validation(m.to_owned()));
	let path = settings.path.as_deref().unwrap_or(DEFAULT_PATH).trim();
	if !path.starts_with('/')
		|| path.len() > 200
		|| path
			.chars()
			.any(|c| c.is_whitespace() || c.is_control() || c == '?' || c == '#')
	{
		return bad("The endpoint path starts with / and has no spaces, ? or #.");
	}
	if settings.transport == Transport::Http && !settings.command.is_empty() {
		return bad("A command only applies to a stdio server; an http server runs its image.");
	}
	if settings.command.len() > MAX_ARGS {
		return bad("A command has at most 64 arguments.");
	}
	for arg in &settings.command {
		if arg.is_empty() || arg.len() > MAX_ARG_LEN || arg.contains(['\n', '\0']) {
			return bad("Each argument is 1 to 1000 characters on one line.");
		}
	}
	Ok(Checked {
		path: path.to_owned(),
		command: (!settings.command.is_empty()).then(|| settings.command.join("\n")),
	})
}

/// What the create call takes.
#[derive(Debug, Clone)]
pub struct CreateInput {
	pub name: String,
	pub slug: Option<String>,
	pub server_id: ServerId,
	pub environment_id: Option<EnvironmentId>,
	/// An image reference, or...
	pub image: Option<String>,
	/// ...a git repository with a Dockerfile.
	pub repo_url: Option<String>,
	pub git_ref: Option<String>,
	pub dockerfile_path: Option<String>,
	pub container_port: Option<i32>,
	pub settings: Settings,
	/// Queue a deploy right away.
	pub deploy: bool,
}

/// A new MCP server: the application behind it, and its settings.
pub struct Created {
	pub app: ApplicationView,
	pub mcp: McpServer,
	/// The deployment queued when asked for.
	pub deploy_id: Option<i64>,
}

fn slug_of(name: &str) -> String {
	let mut slug = String::new();
	for c in name.trim().chars() {
		if c.is_ascii_alphanumeric() {
			slug.push(c.to_ascii_lowercase());
		} else if !slug.is_empty() && !slug.ends_with('-') {
			slug.push('-');
		}
	}
	slug.truncate(60);
	slug.trim_end_matches('-').to_owned()
}

/// Creates the application (an image or a git repository) and marks it as an MCP server.
/// Credentials for a private image or repository are set afterwards on the
/// application's own pages.
///
/// # Errors
/// As [`applications::service::create`], and `Validation` for bad MCP settings.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	input: CreateInput,
) -> AppResult<Created> {
	let checked = check(&input.settings)?;
	let image = input.image.filter(|i| !i.trim().is_empty());
	let repo = input.repo_url.filter(|r| !r.trim().is_empty());
	if image.is_some() == repo.is_some() {
		return Err(ServerError::Validation(
			"Give either an image or a git repository.".to_owned(),
		));
	}
	let slug = input.slug.unwrap_or_else(|| slug_of(&input.name));
	let created = applications::service::create(
		s,
		user,
		team,
		project,
		input.server_id,
		AppInput {
			name: input.name,
			slug,
			repo_url: repo.unwrap_or_default(),
			image,
			registry_username: None,
			registry_password: None,
			registry: Change::Keep,
			git_ref: input.git_ref.unwrap_or_else(|| "main".to_owned()),
			deploy_key: None,
			deploy_key_id: None,
			generate_deploy_key: false,
			git_token: None,
			build_strategy: "dockerfile".to_owned(),
			dockerfile_path: input
				.dockerfile_path
				.unwrap_or_else(|| "Dockerfile".to_owned()),
			container_port: input.container_port.unwrap_or(DEFAULT_PORT),
			memory_limit_mb: None,
			cpu_limit: None,
			environment_id: input.environment_id,
			git_provider_id: None,
			git_repo: String::new(),
		},
	)
	.await?;
	let app = created.app;
	if let Err(e) = mcp_servers::insert(
		&s.db,
		&s.secrets,
		app.id,
		input.settings.transport,
		&checked.path,
		checked.command.as_deref(),
	)
	.await
	{
		if let Err(clean) = applications::service::remove(s, user, team, project, app.id).await {
			tracing::warn!(
				application = app.id.0,
				"failed to clean up a half-made MCP server: {clean}"
			);
		}
		return Err(e);
	}
	audit::log(
		&s.db,
		user,
		team,
		"mcp.create",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set("transport", input.settings.transport.as_str())
			.set("path", checked.path.clone()),
	)
	.await;
	let mcp = mcp_servers::find(&s.db, app.id)
		.await?
		.ok_or(ServerError::Internal)?;
	let deploy_id = if input.deploy {
		Some(
			deployments::service::deploy(s, user, team, project, app.id)
				.await?
				.id
				.0,
		)
	} else {
		None
	};
	Ok(Created {
		app,
		mcp,
		deploy_id,
	})
}

/// Marks an application that already exists as an MCP server (a template deploy does this
/// to the application it made).
///
/// # Errors
/// `Forbidden` without `update application`; `NotFound` for another team's application;
/// `Conflict` when it already is one.
pub async fn register(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	settings: &Settings,
) -> AppResult<McpServer> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let checked = check(settings)?;
	if mcp_servers::find(&s.db, app.id).await?.is_some() {
		return Err(ServerError::Conflict(
			"the application already is an MCP server".to_owned(),
		));
	}
	mcp_servers::insert(
		&s.db,
		&s.secrets,
		app.id,
		settings.transport,
		&checked.path,
		checked.command.as_deref(),
	)
	.await?;
	audit::log(
		&s.db,
		user,
		team,
		"mcp.register",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set("transport", settings.transport.as_str())
			.set("path", checked.path),
	)
	.await;
	mcp_servers::find(&s.db, app.id)
		.await?
		.ok_or(ServerError::Internal)
}

/// An MCP server as callers see it.
pub struct Detail {
	pub app: ApplicationView,
	pub mcp: McpServer,
	/// Whether a deployment of it is running.
	pub running: bool,
}

async fn load(
	s: &AppState,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<(Application, McpServer)> {
	let app = scope::application(&s.db, team, project, id).await?;
	let mcp = mcp_servers::find(&s.db, app.id)
		.await?
		.ok_or(ServerError::ApplicationNotFound)?;
	Ok((app, mcp))
}

/// # Errors
/// `Forbidden` without `read application`; `NotFound` for another team's application or
/// one that is not an MCP server.
pub async fn get(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<Detail> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let (app, mcp) = load(s, team, project, id).await?;
	let view = applications::service::get(&s.db, user, team, project, app.id).await?;
	let running = deployments::latest_running(&s.db, app.id).await?.is_some();
	Ok(Detail {
		app: view,
		mcp,
		running,
	})
}

/// # Errors
/// `Forbidden` without `read application`; `NotFound` for another team's project.
pub async fn list(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<Vec<mcp_servers::Listed>> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	scope::project(&s.db, team, project).await?;
	Ok(mcp_servers::list_in_project(&s.db, project).await?)
}

/// Changes how the server is reached. Takes effect on the next deploy (and, for the path,
/// at once at the gateway).
///
/// # Errors
/// `Forbidden` without `update application`; `NotFound`; `Validation`.
pub async fn update(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
	path: Option<String>,
	command: Option<Vec<String>>,
) -> AppResult<McpServer> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let (app, mcp) = load(s, team, project, id).await?;
	let checked = check(&Settings {
		transport: mcp.transport,
		path: path.or_else(|| Some(mcp.path.clone())),
		command: match command {
			Some(c) => c,
			None => mcp.args(&s.secrets).map_err(|e| {
				tracing::error!(application = app.id.0, "{e}");
				ServerError::Internal
			})?,
		},
	})?;
	mcp_servers::update(
		&s.db,
		&s.secrets,
		app.id,
		&checked.path,
		checked.command.as_deref(),
	)
	.await?;
	links::push_after_change(s, app.server_id).await;
	audit::log(
		&s.db,
		user,
		team,
		"mcp.update",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.set("path", checked.path)
			// The command line may carry a token: the log keeps that it was set and how long.
			.secret("command")
			.set(
				"arguments",
				checked.command.as_deref().map_or(0, |c| c.lines().count()),
			),
	)
	.await;
	mcp_servers::find(&s.db, app.id)
		.await?
		.ok_or(ServerError::Internal)
}

/// The servers an application is linked to, and the ones it could still be linked to.
pub struct AppLinks {
	pub linked: Vec<Link>,
	pub linkable: Vec<(ApplicationId, String)>,
}

/// # Errors
/// `Forbidden` without `read application`; `NotFound` for another team's application.
pub async fn for_app(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<AppLinks> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	Ok(AppLinks {
		linked: mcp_servers::links_of_app(&s.db, app.id).await?,
		linkable: mcp_servers::linkable(&s.db, team, app.server_id, app.id).await?,
	})
}

/// The applications linked to a server.
///
/// # Errors
/// As [`get`].
pub async fn for_server(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<Vec<mcp_servers::LinkedApp>> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let (app, _) = load(s, team, project, id).await?;
	Ok(mcp_servers::apps_of_server(&s.db, app.id).await?)
}

#[derive(sqlx::FromRow)]
struct Reachable {
	id: ApplicationId,
	server_id: ServerId,
	slug: String,
}

/// The MCP server `id` when it belongs to `team`, in any of its projects.
async fn in_team(s: &AppState, team: TeamId, id: ApplicationId) -> AppResult<Reachable> {
	sqlx::query_as(
		"SELECT a.id, a.server_id, a.slug
		FROM mcp_servers m JOIN applications a ON a.id = m.application_id
		JOIN projects p ON p.id = a.project_id
		WHERE a.id = $2 AND p.team_id = $1",
	)
	.bind(team)
	.bind(id)
	.fetch_optional(&s.db)
	.await?
	.ok_or(ServerError::ApplicationNotFound)
}

/// Links `app` to the MCP server `mcp`. The server must be the team's and run on the
/// application's own server.
///
/// # Errors
/// `Forbidden` without `update application`; `NotFound` for another team's application
/// or server; `Validation` for a server on another one or for the application itself.
pub async fn link(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	mcp: ApplicationId,
) -> AppResult<Link> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let target = in_team(s, team, mcp).await?;
	if target.id == app.id {
		return Err(ServerError::Validation(
			"an application cannot be linked to itself".to_owned(),
		));
	}
	if target.server_id != app.server_id {
		return Err(ServerError::Validation(
			"the MCP server runs on a different server than the application; the gateway only \
			reaches MCP servers on the application's own server"
				.to_owned(),
		));
	}
	if mcp_servers::add_link(s, app.id, target.id).await? {
		links::push_after_change(s, app.server_id).await;
		audit::log(
			&s.db,
			user,
			team,
			"app.mcp.link",
			Target::new("application", app.id)
				.named(&app.slug)
				.in_project(project),
			Diff::new()
				.set("mcp_server", target.slug.clone())
				.set("mcp_server_id", target.id.0),
		)
		.await;
	}
	mcp_servers::links_of_app(&s.db, app.id)
		.await?
		.into_iter()
		.find(|l| l.mcp_application_id == target.id)
		.ok_or(ServerError::Internal)
}

/// Removes a link and its key. The variables go from the application's next deploy on; a
/// running container keeps what it was started with, and its key stops working at once.
///
/// # Errors
/// `Forbidden` without `update application`; `NotFound` when the application or the link
/// does not exist.
pub async fn unlink(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	mcp: ApplicationId,
) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	// Bring home what the gateway counted while the key still names the application.
	links::service::collect_best_effort(s, &app).await;
	if !mcp_servers::remove_link(&s.db, app.id, mcp).await? {
		return Err(ServerError::ApplicationNotFound);
	}
	links::push_after_change(s, app.server_id).await;
	audit::log(
		&s.db,
		user,
		team,
		"app.mcp.unlink",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("mcp_server_id", mcp.0),
	)
	.await;
	Ok(())
}

/// Usage of a server over the last 7 days, per key.
///
/// # Errors
/// As [`get`].
pub async fn usage(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<Vec<mcp_servers::KeyUsage>> {
	authorize(&s.db, user, team, Action::Read, Resource::Application).await?;
	let (app, _) = load(s, team, project, id).await?;
	Ok(mcp_servers::usage_for_server(&s.db, app.id).await?)
}

/// One tool of a server. Everything here is the server's own text: it must be escaped
/// by whoever renders it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Tool {
	pub name: String,
	pub description: Option<String>,
	/// Names of the arguments it takes.
	pub params: Vec<String>,
}

/// What `tools/list` returned.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Tools {
	/// The server's own name and version, as `initialize` reported them.
	pub server: Option<String>,
	pub tools: Vec<Tool>,
}

/// The UI's key for the server, issued with the first call. `true` when it is new.
async fn probe_key(s: &AppState, app: ApplicationId) -> AppResult<(String, bool)> {
	let key = format!("pk-mcp-{}", crypto::generate_token(32));
	let sealed = s.secrets.encrypt(&key).map_err(|e| {
		tracing::error!("failed to encrypt the MCP probe key: {e}");
		ServerError::Internal
	})?;
	let issued = sqlx::query(
		"UPDATE mcp_servers SET probe_key_hash = $2, probe_key = $3
		WHERE application_id = $1 AND probe_key_hash IS NULL",
	)
	.bind(app)
	.bind(links::hash_key(&key))
	.bind(sealed)
	.execute(&s.db)
	.await?
	.rows_affected()
		== 1;
	if issued {
		return Ok((key, true));
	}
	let sealed: Vec<u8> =
		sqlx::query_scalar("SELECT probe_key FROM mcp_servers WHERE application_id = $1")
			.bind(app)
			.fetch_one(&s.db)
			.await?;
	let key = s.secrets.decrypt(&sealed).map_err(|e| {
		tracing::error!(application = app.0, "MCP probe key decrypt failed: {e}");
		ServerError::Internal
	})?;
	Ok((key, false))
}

fn parse_tools(line: &str) -> AppResult<Tools> {
	let value: Value = serde_json::from_str(line).map_err(|_| {
		ServerError::Conflict("the MCP server's answer could not be read".to_owned())
	})?;
	if let Some(error) = value.get("error").and_then(Value::as_str) {
		return Err(ServerError::Conflict(format!(
			"the MCP server did not list its tools: {}",
			error.chars().take(300).collect::<String>()
		)));
	}
	let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
	let tools = value
		.get("tools")
		.and_then(Value::as_array)
		.map(|tools| {
			tools
				.iter()
				.take(MAX_TOOLS)
				.filter_map(|tool| {
					let name = text(tool, "name")?;
					let mut params: Vec<String> = tool
						.pointer("/inputSchema/properties")
						.and_then(Value::as_object)
						.map(|p| p.keys().cloned().collect())
						.unwrap_or_default();
					params.truncate(16);
					Some(Tool {
						name: name.chars().take(120).collect(),
						description: text(tool, "description")
							.map(|d| d.chars().take(MAX_DESCRIPTION).collect()),
						params,
					})
				})
				.collect()
		})
		.unwrap_or_default();
	let server = value.get("server").and_then(|s| {
		let name = text(s, "name")?;
		Some(match text(s, "version") {
			Some(version) => format!("{name} {version}"),
			None => name,
		})
	});
	Ok(Tools { server, tools })
}

/// Asks the running server for its tools (`initialize`, then `tools/list`) through the
/// gateway, from a throwaway container on the server's network.
///
/// # Errors
/// `Forbidden` without `update application`; `NotFound`; `Conflict` unless the server is
/// running and answers.
pub async fn tools(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<Tools> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	let (app, _) = load(s, team, project, id).await?;
	if deployments::latest_running(&s.db, app.id).await?.is_none() {
		return Err(ServerError::Conflict(
			"the MCP server is not running; deploy it first".to_owned(),
		));
	}
	let server = scope::server(&s.db, team, app.server_id).await?;
	let client = s.agents.client(s, &server).await?;
	let (key, issued) = probe_key(s, app.id).await?;
	if issued {
		links::push(s, &client, server.id).await?;
	}
	let request = GatewayMcpToolsRequest {
		key,
		name: app.slug.clone(),
		timeout_s: PROBE_TIMEOUT_S,
	};
	let mut events = client
		.stream(Method::POST, "/v1/gateway/mcp/tools", Some(&request))
		.await?;
	let mut answer = None;
	while let Some(event) = events.next().await {
		match event {
			Ok(Event::Log { line, .. }) => answer = Some(line),
			Ok(Event::Failed { message }) => {
				return Err(ServerError::Conflict(format!(
					"could not reach the MCP server: {message}"
				)));
			}
			Ok(_) => {}
			Err(e) => {
				tracing::warn!(application = app.id.0, error = %e, "MCP tools stream failed");
				return Err(ServerError::Conflict(
					"the connection to the server agent was lost".to_owned(),
				));
			}
		}
	}
	// The probe's own request counts against its key; bring it home now.
	collect_usage_best_effort(s, &server).await;
	parse_tools(
		&answer.ok_or_else(|| ServerError::Conflict("the MCP server gave no answer".to_owned()))?,
	)
}

async fn collect_usage_best_effort(s: &AppState, server: &servers::Server) {
	let result = async {
		let client = s.agents.client(s, server).await?;
		links::collect_usage(s, &client).await
	}
	.await;
	if let Err(e) = result {
		tracing::debug!(server = %server.id, error = %e, "could not collect MCP usage after listing tools");
	}
}

/// Whether `app` is an MCP server. For pages that already loaded the application.
pub async fn is_server(db: &PgPool, app: ApplicationId) -> bool {
	mcp_servers::find(db, app).await.ok().flatten().is_some()
}

/// Whether the application may be removed: not while others are linked to it.
///
/// # Errors
/// `Conflict` naming how many applications use the server.
pub async fn ensure_removable(db: &PgPool, app: ApplicationId) -> AppResult<()> {
	let linked = mcp_servers::count_links(db, app).await?;
	if linked > 0 {
		return Err(ServerError::Conflict(format!(
			"{linked} application(s) are linked to this MCP server; unlink them first"
		)));
	}
	Ok(())
}

#[cfg(test)]
mod unit {
	use super::*;

	fn settings(transport: Transport, path: Option<&str>, command: &[&str]) -> Settings {
		Settings {
			transport,
			path: path.map(str::to_owned),
			command: command.iter().map(|c| (*c).to_owned()).collect(),
		}
	}

	#[test]
	fn mcp_resource_settings_are_checked() {
		let ok = check(&settings(Transport::Http, None, &[])).unwrap();
		assert_eq!((ok.path.as_str(), ok.command), ("/mcp", None));
		let stdio = check(&settings(Transport::Stdio, Some("/x"), &["node", "a b"])).unwrap();
		assert_eq!(stdio.command.as_deref(), Some("node\na b"));
		assert_eq!(
			mcp_servers::args_of(stdio.command.as_deref()),
			["node", "a b"]
		);
		for bad in [
			settings(Transport::Http, Some("mcp"), &[]),
			settings(Transport::Http, Some("/a b"), &[]),
			settings(Transport::Http, Some("/a?b"), &[]),
			settings(Transport::Http, None, &["x"]),
			settings(Transport::Stdio, None, &[""]),
			settings(Transport::Stdio, None, &["a\nb"]),
		] {
			assert!(check(&bad).is_err(), "{bad:?}");
		}
	}

	#[test]
	fn mcp_resource_names_become_slugs_and_env_names() {
		assert_eq!(slug_of("  My Files! "), "my-files");
		assert_eq!(slug_of("---"), "");
		assert_eq!(mcp_servers::env_name("my-files"), "MCP_MY_FILES");
		assert_eq!(mcp_servers::alias("fetch"), "mcp-fetch");
		assert_eq!(
			mcp_servers::gateway_url("fetch"),
			"http://llm.podkit.internal:8088/mcp/fetch"
		);
	}

	#[test]
	fn mcp_resource_tools_are_read_from_the_probe_line() {
		let line = serde_json::json!({
			"server": {"name": "files", "version": "1.2"},
			"tools": [
				{"name": "read", "description": "Reads", "inputSchema": {"properties": {"path": {}, "n": {}}}},
				{"description": "no name"},
				{"name": "ping"},
			],
		})
		.to_string();
		let tools = parse_tools(&line).unwrap();
		assert_eq!(tools.server.as_deref(), Some("files 1.2"));
		assert_eq!(tools.tools.len(), 2);
		assert_eq!(tools.tools[0].params, ["n", "path"]);
		assert_eq!(tools.tools[1].description, None);
		assert!(parse_tools(r#"{"error":"HTTP 401: nope"}"#).is_err());
		assert!(parse_tools("garbage").is_err());
	}
}
