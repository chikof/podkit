//! HTTP handlers of the MCP server slice. They only translate: the rules live in
//! [`super::service`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, CreateInput, Settings, Tools};
use super::{Link, Listed, McpServer, Transport};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, ServerId, TeamId};
use crate::state::AppState;

/// Nested under `/teams/{team}/projects/{project}/mcp-servers`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list).post(create))
		.route("/{id}", get(show).put(update))
		.route("/{id}/tools", post(tools))
		.route("/{id}/links", get(server_links))
		.route("/{id}/usage", get(usage))
}

/// Nested under an application's path, at `/mcp`.
pub fn app_router() -> Router<AppState> {
	Router::new()
		.route("/", get(app_links).post(link))
		.route("/{mcp_id}", delete(unlink))
}

type ProjectPath = Path<(TeamId, ProjectId)>;

#[derive(Serialize)]
struct ServerResponse {
	id: ApplicationId,
	name: String,
	slug: String,
	transport: Transport,
	path: String,
	/// stdio only.
	command: Vec<String>,
	server_id: ServerId,
	/// Where linked applications call it.
	gateway_url: String,
	running: bool,
}

impl ServerResponse {
	fn new(detail: &service::Detail) -> Self {
		Self {
			id: detail.app.id,
			name: detail.app.name.clone(),
			slug: detail.app.slug.clone(),
			transport: detail.mcp.transport,
			path: detail.mcp.path.clone(),
			command: detail.mcp.args(),
			server_id: detail.app.server_id,
			gateway_url: super::gateway_url(&detail.app.slug),
			running: detail.running,
		}
	}
}

#[derive(Serialize)]
struct ListedResponse {
	id: ApplicationId,
	name: String,
	slug: String,
	transport: Transport,
	server_id: ServerId,
	links: i64,
	gateway_url: String,
}

impl From<Listed> for ListedResponse {
	fn from(l: Listed) -> Self {
		Self {
			gateway_url: super::gateway_url(&l.slug),
			id: l.application_id,
			name: l.name,
			slug: l.slug,
			transport: l.transport,
			server_id: l.server_id,
			links: l.links,
		}
	}
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): ProjectPath,
) -> AppResult<Json<Vec<ListedResponse>>> {
	let rows = service::list(&s, user.id, team, project).await?;
	Ok(Json(rows.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize)]
struct CreateRequest {
	name: String,
	slug: Option<String>,
	server_id: ServerId,
	environment_id: Option<EnvironmentId>,
	image: Option<String>,
	repo_url: Option<String>,
	git_ref: Option<String>,
	dockerfile_path: Option<String>,
	container_port: Option<i32>,
	/// `http` or `stdio`.
	transport: String,
	path: Option<String>,
	#[serde(default)]
	command: Vec<String>,
	/// Queue a deploy right away.
	#[serde(default)]
	deploy: bool,
}

#[derive(Serialize)]
struct CreateResponse {
	#[serde(flatten)]
	server: ServerResponse,
	deployment_id: Option<i64>,
}

fn transport_of(s: &str) -> AppResult<Transport> {
	Transport::parse(s)
		.ok_or_else(|| ServerError::Validation("transport is http or stdio".to_owned()))
}

async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): ProjectPath,
	Json(body): Json<CreateRequest>,
) -> AppResult<(StatusCode, Json<CreateResponse>)> {
	let transport = transport_of(&body.transport)?;
	let created = service::create(
		&s,
		user.id,
		team,
		project,
		CreateInput {
			name: body.name,
			slug: body.slug,
			server_id: body.server_id,
			environment_id: body.environment_id,
			image: body.image,
			repo_url: body.repo_url,
			git_ref: body.git_ref,
			dockerfile_path: body.dockerfile_path,
			container_port: body.container_port,
			settings: Settings {
				transport,
				path: body.path,
				command: body.command,
			},
			deploy: body.deploy,
		},
	)
	.await?;
	let detail = service::Detail {
		app: created.app,
		mcp: created.mcp,
		running: false,
	};
	Ok((
		StatusCode::CREATED,
		Json(CreateResponse {
			server: ServerResponse::new(&detail),
			deployment_id: created.deploy_id,
		}),
	))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<ServerResponse>> {
	let detail = service::get(&s, user.id, team, project, id).await?;
	Ok(Json(ServerResponse::new(&detail)))
}

#[derive(Deserialize)]
struct UpdateRequest {
	path: Option<String>,
	command: Option<Vec<String>>,
}

async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<UpdateRequest>,
) -> AppResult<Json<ServerResponse>> {
	let _: McpServer =
		service::update(&s, user.id, team, project, id, body.path, body.command).await?;
	let detail = service::get(&s, user.id, team, project, id).await?;
	Ok(Json(ServerResponse::new(&detail)))
}

async fn tools(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Tools>> {
	Ok(Json(service::tools(&s, user.id, team, project, id).await?))
}

#[derive(Serialize)]
struct LinkedAppResponse {
	application_id: ApplicationId,
	project_id: ProjectId,
	name: String,
}

async fn server_links(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<LinkedAppResponse>>> {
	let rows = service::for_server(&s, user.id, team, project, id).await?;
	Ok(Json(
		rows.into_iter()
			.map(|a| LinkedAppResponse {
				application_id: a.application_id,
				project_id: a.project_id,
				name: a.name,
			})
			.collect(),
	))
}

#[derive(Serialize)]
struct UsageResponse {
	key: String,
	requests_24h: i64,
	requests_7d: i64,
}

async fn usage(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<UsageResponse>>> {
	let rows = service::usage(&s, user.id, team, project, id).await?;
	Ok(Json(
		rows.into_iter()
			.map(|u| UsageResponse {
				key: u.key_label,
				requests_24h: u.requests_24h,
				requests_7d: u.requests_7d,
			})
			.collect(),
	))
}

/// A link as the application sees it. No key: it is only ever handed to the
/// application's own deploys.
#[derive(Serialize)]
struct LinkResponse {
	mcp_server_id: ApplicationId,
	name: String,
	slug: String,
	transport: Transport,
	/// The variable names the application gets.
	url_variable: String,
	key_variable: String,
	gateway_url: String,
}

impl From<Link> for LinkResponse {
	fn from(l: Link) -> Self {
		let prefix = super::env_name(&l.slug);
		Self {
			mcp_server_id: l.mcp_application_id,
			gateway_url: super::gateway_url(&l.slug),
			url_variable: format!("{prefix}_URL"),
			key_variable: format!("{prefix}_KEY"),
			name: l.name,
			slug: l.slug,
			transport: l.transport,
		}
	}
}

type AppPath = Path<(TeamId, ProjectId, ApplicationId)>;

async fn app_links(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): AppPath,
) -> AppResult<Json<Vec<LinkResponse>>> {
	let links = service::for_app(&s, user.id, team, project, app).await?;
	Ok(Json(links.linked.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize)]
struct LinkRequest {
	mcp_server_id: ApplicationId,
}

async fn link(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): AppPath,
	Json(body): Json<LinkRequest>,
) -> AppResult<(StatusCode, Json<LinkResponse>)> {
	let link = service::link(&s, user.id, team, project, app, body.mcp_server_id).await?;
	Ok((StatusCode::CREATED, Json(link.into())))
}

async fn unlink(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app, mcp)): Path<(TeamId, ProjectId, ApplicationId, ApplicationId)>,
) -> AppResult<StatusCode> {
	service::unlink(&s, user.id, team, project, app, mcp).await?;
	Ok(StatusCode::NO_CONTENT)
}
