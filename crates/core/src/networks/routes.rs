use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, NetworkView, ResourceNetworks};
use super::{Member, Network, ResourceKind, migrate};
use crate::auth::AuthUser;
use crate::error::{AppResult, ServerError};
use crate::ids::{NetworkId, ServerId, TeamId};
use crate::state::AppState;

/// Mounted at `/teams/{team_id}/servers/{server_id}/networks`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", get(list).post(create))
		.route("/scan", get(scan))
		.route("/import", post(import))
		.route("/migrate", post(migrate_server))
}

/// Mounted at `/teams/{team_id}/networks`.
pub fn network_router() -> Router<AppState> {
	Router::new()
		.route("/{network_id}", get(show).patch(rename).delete(remove))
		.route("/{network_id}/members", post(attach))
		.route("/{network_id}/members/{kind}/{resource_id}", delete(detach))
		.route("/of/{kind}/{resource_id}", get(of_resource))
}

#[derive(Serialize)]
struct NetworkResponse {
	id: NetworkId,
	server_id: ServerId,
	name: String,
	slug: String,
	kind: String,
	environment_id: Option<i64>,
	podman_name: String,
	internal: bool,
	imported: bool,
	created_at: String,
}

impl From<&Network> for NetworkResponse {
	fn from(n: &Network) -> Self {
		Self {
			id: n.id,
			server_id: n.server_id,
			name: n.name.clone(),
			slug: n.slug.clone(),
			kind: n.kind.clone(),
			environment_id: n.environment_id.map(|e| e.0),
			podman_name: n.podman_name.clone(),
			internal: n.internal,
			imported: n.imported,
			created_at: n.created_at.to_string(),
		}
	}
}

#[derive(Serialize)]
struct MemberResponse {
	kind: String,
	id: i64,
	name: String,
	project_id: i64,
	aliases: Vec<String>,
}

impl From<&Member> for MemberResponse {
	fn from(m: &Member) -> Self {
		Self {
			kind: m.resource_kind.clone(),
			id: m.resource_id,
			name: m.name.clone(),
			project_id: m.project_id,
			aliases: m.aliases.clone(),
		}
	}
}

#[derive(Serialize)]
struct ViewResponse {
	#[serde(flatten)]
	network: NetworkResponse,
	members: Vec<MemberResponse>,
}

impl From<&NetworkView> for ViewResponse {
	fn from(v: &NetworkView) -> Self {
		Self {
			network: NetworkResponse::from(&v.network),
			members: v.members.iter().map(MemberResponse::from).collect(),
		}
	}
}

#[derive(Deserialize)]
struct CreateRequest {
	name: String,
	#[serde(default)]
	internal: bool,
}

#[derive(Deserialize)]
struct ImportRequest {
	podman_name: String,
	#[serde(default)]
	name: Option<String>,
}

#[derive(Deserialize)]
struct RenameRequest {
	name: String,
}

#[derive(Deserialize)]
struct AttachRequest {
	kind: String,
	id: i64,
	#[serde(default)]
	aliases: Vec<String>,
}

fn kind(s: &str) -> AppResult<ResourceKind> {
	ResourceKind::parse(s).ok_or(ServerError::NotFound)
}

async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<Vec<ViewResponse>>> {
	let views = service::list(&s, user.id, team, server).await?;
	Ok(Json(views.iter().map(ViewResponse::from).collect()))
}

async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
	Json(body): Json<CreateRequest>,
) -> AppResult<(StatusCode, Json<NetworkResponse>)> {
	let n = service::create(&s, user.id, team, server, &body.name, body.internal).await?;
	Ok((StatusCode::CREATED, Json(NetworkResponse::from(&n))))
}

#[derive(Serialize)]
struct Importable {
	name: String,
	internal: bool,
	containers: usize,
}

async fn scan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<Vec<Importable>>> {
	let found = service::scan(&s, user.id, team, server).await?;
	Ok(Json(
		found
			.into_iter()
			.map(|n| Importable {
				name: n.name,
				internal: n.internal,
				containers: n.containers.len(),
			})
			.collect(),
	))
}

async fn import(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
	Json(body): Json<ImportRequest>,
) -> AppResult<(StatusCode, Json<NetworkResponse>)> {
	let n = service::import(
		&s,
		user.id,
		team,
		server,
		&body.podman_name,
		body.name.as_deref(),
	)
	.await?;
	Ok((StatusCode::CREATED, Json(NetworkResponse::from(&n))))
}

#[derive(Serialize)]
struct MigrateResponse {
	moved: Vec<String>,
	redeploy: Vec<String>,
}

async fn migrate_server(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<MigrateResponse>> {
	let report = migrate::run(&s, user.id, team, server).await?;
	Ok(Json(MigrateResponse {
		moved: report.moved,
		redeploy: report.redeploy,
	}))
}

async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NetworkId)>,
) -> AppResult<Json<ViewResponse>> {
	let v = service::get(&s, user.id, team, id).await?;
	Ok(Json(ViewResponse::from(&v)))
}

async fn rename(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NetworkId)>,
	Json(body): Json<RenameRequest>,
) -> AppResult<Json<NetworkResponse>> {
	let n = service::rename(&s, user.id, team, id, &body.name).await?;
	Ok(Json(NetworkResponse::from(&n)))
}

async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NetworkId)>,
) -> AppResult<StatusCode> {
	service::delete(&s, user.id, team, id).await?;
	Ok(StatusCode::NO_CONTENT)
}

async fn attach(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, NetworkId)>,
	Json(body): Json<AttachRequest>,
) -> AppResult<StatusCode> {
	service::attach(
		&s,
		user.id,
		team,
		id,
		kind(&body.kind)?,
		body.id,
		&body.aliases,
	)
	.await?;
	Ok(StatusCode::NO_CONTENT)
}

async fn detach(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id, k, resource)): Path<(TeamId, NetworkId, String, i64)>,
) -> AppResult<StatusCode> {
	service::detach(&s, user.id, team, id, kind(&k)?, resource).await?;
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct JoinedResponse {
	#[serde(flatten)]
	network: NetworkResponse,
	aliases: Vec<String>,
}

#[derive(Serialize)]
struct OfResponse {
	joined: Vec<JoinedResponse>,
	available: Vec<NetworkResponse>,
}

impl From<&ResourceNetworks> for OfResponse {
	fn from(r: &ResourceNetworks) -> Self {
		Self {
			joined: r
				.joined
				.iter()
				.map(|(n, aliases)| JoinedResponse {
					network: NetworkResponse::from(n),
					aliases: aliases.clone(),
				})
				.collect(),
			available: r.available.iter().map(NetworkResponse::from).collect(),
		}
	}
}

async fn of_resource(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, k, resource)): Path<(TeamId, String, i64)>,
) -> AppResult<Json<OfResponse>> {
	let r = service::for_resource(&s, user.id, team, kind(&k)?, resource).await?;
	Ok(Json(OfResponse::from(&r)))
}
