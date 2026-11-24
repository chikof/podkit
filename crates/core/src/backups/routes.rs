//! Backups of one database, under
//! `/api/v1/teams/{team}/projects/{project}/databases/{database}/backups`.
//! The use cases are in [`super::service`].

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service;
use super::{Backup, BackupStatus};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{BackupId, BackupTargetId, DatabaseId, ProjectId, TeamId, UserId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{backup_id}", delete(remove))
		.route("/{backup_id}/restore", post(restore))
}

#[derive(Serialize)]
pub struct BackupResponse {
	pub id: BackupId,
	pub database_id: DatabaseId,
	pub target_id: Option<BackupTargetId>,
	pub status: BackupStatus,
	pub object_key: Option<String>,
	pub size_bytes: Option<i64>,
	pub sha256: Option<String>,
	pub error: Option<String>,
	pub triggered_by: Option<UserId>,
	pub created_at: String,
	pub finished_at: Option<String>,
}

impl From<&Backup> for BackupResponse {
	fn from(b: &Backup) -> Self {
		Self {
			id: b.id,
			database_id: b.database_id,
			target_id: b.target_id,
			status: b.status,
			object_key: b.object_key.clone(),
			size_bytes: b.size_bytes,
			sha256: b.sha256.clone(),
			error: b.error.clone(),
			triggered_by: b.triggered_by,
			created_at: b.created_at.to_string(),
			finished_at: b.finished_at.map(|t| t.to_string()),
		}
	}
}

#[derive(Serialize)]
struct DeleteBackupResponse {
	id: BackupId,
}

#[derive(Deserialize)]
struct ListParams {
	limit: Option<i64>,
	before: Option<BackupId>,
}

/// Queues a backup of the database to its configured target and returns
/// immediately. Poll the list for the outcome. Caller must hold `create
/// backup` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<(StatusCode, Json<BackupResponse>)> {
	let backup = service::create(&s, user.id, team, project, database).await?;
	Ok((StatusCode::ACCEPTED, Json(BackupResponse::from(&backup))))
}

/// Lists a database's backups, newest first, paged with `?limit=` (1..=200,
/// default 50) and `?before=<backup id>`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<BackupResponse>>> {
	let rows = service::list(
		&s.db,
		user.id,
		team,
		project,
		database,
		params.before,
		params.limit,
	)
	.await?;
	Ok(Json(rows.iter().map(BackupResponse::from).collect()))
}

/// Queues a restore of a succeeded backup into the running database. The
/// dump replaces the database's current contents. Caller must hold `update
/// database` on the team.
async fn restore(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database, id)): Path<(TeamId, ProjectId, DatabaseId, BackupId)>,
) -> AppResult<(StatusCode, Json<BackupResponse>)> {
	let backup = service::restore(&s, user.id, team, project, database, id).await?;
	Ok((StatusCode::ACCEPTED, Json(BackupResponse::from(&backup))))
}

/// Deletes the backup's object from its target, then the row. A backup that
/// is still queued or running cannot be deleted. Caller must hold `delete
/// backup` on the team.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, database, id)): Path<(TeamId, ProjectId, DatabaseId, BackupId)>,
) -> AppResult<Json<DeleteBackupResponse>> {
	let id = service::remove(&s, user.id, team, project, database, id).await?;
	Ok(Json(DeleteBackupResponse { id }))
}
