//! Backups of one database, under
//! `/api/v1/teams/{team}/projects/{project}/databases/{database}/backups`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use proto::Engine;
use serde::{Deserialize, Serialize};

use super::{Backup, BackupStatus};
use crate::auth::AuthUser;
use crate::auth::permission::{Action, Resource, authorize};
use crate::databases::DatabaseStatus;
use crate::error::{AppResult, ServerError};
use crate::ids::{BackupId, BackupTargetId, DatabaseId, ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::{backups, scope};

const DEFAULT_PAGE_SIZE: i64 = 50;
const MAX_PAGE_SIZE: i64 = 200;

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
	authorize(&s.db, user.id, team, Action::Create, Resource::Backup).await?;
	let database = scope::database(&s.db, team, project, database).await?;

	if database.engine() == Engine::Redis {
		return Err(ServerError::Validation(
			"redis backups are not supported".to_owned(),
		));
	}
	let target = database
		.backup_target_id
		.ok_or_else(|| ServerError::Validation("configure a backup target first".to_owned()))?;

	let backup = backups::queue(&s, database.id, database.server_id, target, Some(user.id)).await?;
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
	authorize(&s.db, user.id, team, Action::Read, Resource::Backup).await?;
	let database = scope::database(&s.db, team, project, database).await?;

	let limit = params
		.limit
		.unwrap_or(DEFAULT_PAGE_SIZE)
		.clamp(1, MAX_PAGE_SIZE);
	let rows = backups::list(&s.db, database.id, params.before, limit).await?;
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
	authorize(&s.db, user.id, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	let backup = backups::find(&s.db, database.id, id)
		.await?
		.ok_or(ServerError::BackupNotFound)?;

	if backup.status != BackupStatus::Succeeded {
		return Err(ServerError::Validation(
			"only a succeeded backup can be restored".to_owned(),
		));
	}
	if database.status != DatabaseStatus::Running {
		return Err(ServerError::Validation(
			"the database must be running to restore a backup".to_owned(),
		));
	}

	jobs::enqueue(
		&s,
		JobKind::Restore,
		database.server_id,
		backup.id.0,
		format!("db:{}", database.id),
	)
	.await?;
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
	authorize(&s.db, user.id, team, Action::Delete, Resource::Backup).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	let backup = backups::find(&s.db, database.id, id)
		.await?
		.ok_or(ServerError::BackupNotFound)?;

	if matches!(backup.status, BackupStatus::Queued | BackupStatus::Running) {
		return Err(ServerError::Conflict("backup is in progress".to_owned()));
	}
	backups::purge(&s, &backup).await?;
	Ok(Json(DeleteBackupResponse { id: backup.id }))
}
