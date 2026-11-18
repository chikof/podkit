//! Backup targets: the S3-compatible buckets of a team. Routes live under
//! `/api/v1/teams/{team}/backup-targets`; the use cases are in [`service`].

use axum::extract::{Path, State};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::BackupTarget;
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{BackupTargetId, TeamId};
use crate::state::AppState;

pub mod service;

pub(crate) use service::list_for_team;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{target_id}", delete(remove))
}

#[derive(Deserialize)]
struct CreateTargetRequest {
	name: String,
	endpoint: String,
	region: String,
	bucket: String,
	#[serde(default)]
	path_style: bool,
	#[serde(default)]
	prefix: String,
	access_key_id: String,
	secret_access_key: String,
}

/// No `secret_access_key`: it round-trips out exactly once, at write time.
#[derive(Serialize)]
struct TargetResponse {
	id: BackupTargetId,
	name: String,
	endpoint: String,
	region: String,
	bucket: String,
	path_style: bool,
	prefix: String,
	access_key_id: String,
	created_at: String,
}

impl From<&BackupTarget> for TargetResponse {
	fn from(t: &BackupTarget) -> Self {
		Self {
			id: t.id,
			name: t.name.clone(),
			endpoint: t.endpoint.clone(),
			region: t.region.clone(),
			bucket: t.bucket.clone(),
			path_style: t.path_style,
			prefix: t.prefix.clone(),
			access_key_id: t.access_key_id.clone(),
			created_at: t.created_at.to_string(),
		}
	}
}

#[derive(Serialize)]
struct DeleteTargetResponse {
	id: BackupTargetId,
}

/// Registers a bucket for the team. The secret is encrypted at rest and never
/// returned. Caller must hold `create backup` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Json(body): Json<CreateTargetRequest>,
) -> AppResult<Json<TargetResponse>> {
	let input = service::CreateInput {
		name: body.name,
		endpoint: body.endpoint,
		region: body.region,
		bucket: body.bucket,
		path_style: body.path_style,
		prefix: body.prefix,
		access_key_id: body.access_key_id,
		secret_access_key: body.secret_access_key,
	};
	let target = service::create(&s, user.id, team, input).await?;
	Ok(Json(TargetResponse::from(&target)))
}

/// Lists the team's backup targets without their secrets; caller must hold
/// `read backup` on the team.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
) -> AppResult<Json<Vec<TargetResponse>>> {
	let targets = service::list(&s.db, user.id, team).await?;
	Ok(Json(targets.iter().map(TargetResponse::from).collect()))
}

/// Deletes a target. Databases using it lose their target and backups made
/// to it keep their rows but can no longer be restored. Caller must hold
/// `delete backup` on the team.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, id)): Path<(TeamId, BackupTargetId)>,
) -> AppResult<Json<DeleteTargetResponse>> {
	let id = service::remove(&s.db, user.id, team, id).await?;
	Ok(Json(DeleteTargetResponse { id }))
}
