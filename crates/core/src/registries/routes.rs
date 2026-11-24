use axum::extract::{Path, State};
use axum::routing::{patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::Registry;
use super::service::{self, CreateInput, LoginOutcome, UpdateInput};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{RegistryId, ServerId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/test", post(test_new))
		.route("/{registry_id}", patch(update).delete(remove))
		.route("/{registry_id}/test", post(test))
}

#[derive(Deserialize)]
struct CreateRegistryRequest {
	name: String,
	url: String,
	username: String,
	password: String,
	/// `test` only: the server whose agent logs in. The team's local server by default.
	#[serde(default)]
	server_id: Option<ServerId>,
}

#[derive(Deserialize)]
struct UpdateRegistryRequest {
	name: String,
	username: String,
	/// Omitted or empty keeps the stored password.
	#[serde(default)]
	password: Option<String>,
}

#[derive(Deserialize, Default)]
struct TestRequest {
	#[serde(default)]
	server_id: Option<ServerId>,
}

/// No `password`: it goes in once and never comes out.
#[derive(Serialize)]
struct RegistryResponse {
	id: RegistryId,
	name: String,
	url: String,
	username: String,
	created_at: String,
}

impl From<&Registry> for RegistryResponse {
	fn from(r: &Registry) -> Self {
		Self {
			id: r.id,
			name: r.name.clone(),
			url: r.url.clone(),
			username: r.username.clone(),
			created_at: r.created_at.to_string(),
		}
	}
}

#[derive(Serialize)]
struct DeleteRegistryResponse {
	id: RegistryId,
}

#[derive(Serialize)]
struct LoginResponse {
	ok: bool,
	message: String,
}

impl From<LoginOutcome> for LoginResponse {
	fn from(o: LoginOutcome) -> Self {
		Self {
			ok: o.ok,
			message: o.message,
		}
	}
}

/// Saves a login; caller must hold `create registry` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateRegistryRequest>,
) -> AppResult<Json<RegistryResponse>> {
	let input = CreateInput {
		name: body.name,
		url: body.url,
		username: body.username,
		password: body.password,
	};
	let registry = service::create(&s, user.id, team, input).await?;
	Ok(Json(RegistryResponse::from(&registry)))
}

/// The team's registries without their passwords; caller must hold `read registry`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<RegistryResponse>>> {
	let registries = service::list(&s.db, user.id, team).await?;
	Ok(Json(
		registries.iter().map(RegistryResponse::from).collect(),
	))
}

/// Renames a registry or rotates its login; caller must hold `update registry`.
async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, RegistryId)>,
	Json(body): Json<UpdateRegistryRequest>,
) -> AppResult<Json<RegistryResponse>> {
	let input = UpdateInput {
		name: body.name,
		username: body.username,
		password: body.password,
	};
	let registry = service::update(&s, user.id, team, id, input).await?;
	Ok(Json(RegistryResponse::from(&registry)))
}

/// Deletes a registry no application or stack pulls with; caller must hold
/// `delete registry`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, RegistryId)>,
) -> AppResult<Json<DeleteRegistryResponse>> {
	let id = service::remove(&s.db, user.id, team, id).await?;
	Ok(Json(DeleteRegistryResponse { id }))
}

/// Logs in with the saved login from a server's agent; caller must hold
/// `update registry`. A refused login is a normal answer with `ok: false`.
async fn test(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, RegistryId)>,
	body: Option<Json<TestRequest>>,
) -> AppResult<Json<LoginResponse>> {
	let server = body.and_then(|Json(b)| b.server_id);
	let outcome = service::test(&s, user.id, team, id, server).await?;
	Ok(Json(outcome.into()))
}

/// Tries a login before saving it; stores nothing. Caller must hold `create registry`.
async fn test_new(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateRegistryRequest>,
) -> AppResult<Json<LoginResponse>> {
	let server = body.server_id;
	let input = CreateInput {
		name: body.name,
		url: body.url,
		username: body.username,
		password: body.password,
	};
	let outcome = service::test_new(&s, user.id, team, server, input).await?;
	Ok(Json(outcome.into()))
}
