//! Backup use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or the agent.

use proto::Engine;
use sqlx::PgPool;

use super::{Backup, BackupStatus};
use crate::auth::permission::{Action, Resource, authorize};
use crate::auth::users;
use crate::backups;
use crate::databases::{Database, DatabaseStatus};
use crate::error::{AppResult, ServerError};
use crate::ids::{BackupId, DatabaseId, ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::scope;
use crate::state::AppState;

pub const DEFAULT_PAGE_SIZE: i64 = 50;
pub const MAX_PAGE_SIZE: i64 = 200;

/// One backup as the UI lists it: the row and the name of who started it.
#[derive(Debug, Clone)]
pub struct Entry {
	pub backup: Backup,
	/// `None` for a scheduled backup, or when the user no longer exists.
	pub triggered_by: Option<String>,
}

/// What the Backups tab needs to show and to decide which buttons to offer.
#[derive(Debug, Clone)]
pub struct Overview {
	pub entries: Vec<Entry>,
	/// Whether a backup of this database can be started now: it has a target
	/// and its engine is supported.
	pub can_start: Result<(), String>,
}

/// Queues a backup of the database to its configured target and returns
/// immediately. Caller must hold `create backup` on the team.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
) -> AppResult<Backup> {
	authorize(&s.db, user, team, Action::Create, Resource::Backup).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	startable(&database)?;
	let target = database
		.backup_target_id
		.ok_or_else(|| ServerError::Validation("configure a backup target first".to_owned()))?;
	backups::queue(s, database.id, database.server_id, target, Some(user)).await
}

fn startable(database: &Database) -> Result<(), ServerError> {
	if database.engine() == Engine::Redis {
		return Err(ServerError::Validation(
			"redis backups are not supported".to_owned(),
		));
	}
	Ok(())
}

/// A database's backups, newest first, paged with `before` (a backup id) and
/// `limit` (1..=200, default 50). Caller must hold `read backup` on the team.
pub async fn list(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
	before: Option<BackupId>,
	limit: Option<i64>,
) -> AppResult<Vec<Backup>> {
	authorize(db, user, team, Action::Read, Resource::Backup).await?;
	let database = scope::database(db, team, project, database).await?;
	let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
	Ok(backups::list(db, database.id, before, limit).await?)
}

/// The Backups tab: the newest backups with who started each, and whether a
/// new one can be started. Caller must hold `read backup` on the team.
pub async fn overview(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
) -> AppResult<Overview> {
	authorize(db, user, team, Action::Read, Resource::Backup).await?;
	let database = scope::database(db, team, project, database).await?;
	let rows = backups::list(db, database.id, None, DEFAULT_PAGE_SIZE).await?;

	let mut names: Vec<(UserId, Option<String>)> = Vec::new();
	let mut entries = Vec::with_capacity(rows.len());
	for backup in rows {
		let triggered_by = match backup.triggered_by {
			None => None,
			Some(id) => {
				if let Some((_, name)) = names.iter().find(|(known, _)| *known == id) {
					name.clone()
				} else {
					let name = users::name_of(db, id).await?;
					names.push((id, name.clone()));
					name
				}
			}
		};
		entries.push(Entry {
			backup,
			triggered_by,
		});
	}

	let can_start = startable(&database)
		.and_then(|()| {
			database.backup_target_id.map(|_| ()).ok_or_else(|| {
				ServerError::Validation("configure a backup target first".to_owned())
			})
		})
		.map_err(|e| e.to_string());
	Ok(Overview { entries, can_start })
}

/// Queues a restore of a succeeded backup into the running database. The
/// dump replaces the database's current contents. Caller must hold `update
/// database` on the team.
pub async fn restore(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
	id: BackupId,
) -> AppResult<Backup> {
	authorize(&s.db, user, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	queue_restore(s, &database, id).await
}

/// [`restore`], for callers that make the user confirm first: nothing is
/// queued unless `typed` is exactly the database's name. The check lives here
/// so a forged request cannot skip it.
pub async fn restore_confirmed(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
	id: BackupId,
	typed: &str,
) -> AppResult<Backup> {
	authorize(&s.db, user, team, Action::Update, Resource::Database).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	if typed != database.name {
		return Err(ServerError::Validation(
			"The name doesn't match. Type the database name exactly to restore.".to_owned(),
		));
	}
	queue_restore(s, &database, id).await
}

async fn queue_restore(s: &AppState, database: &Database, id: BackupId) -> AppResult<Backup> {
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
		s,
		JobKind::Restore,
		database.server_id,
		backup.id.0,
		format!("db:{}", database.id),
	)
	.await?;
	Ok(backup)
}

/// Deletes the backup's object from its target, then the row. A backup that
/// is still queued or running cannot be deleted. Caller must hold `delete
/// backup` on the team.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	database: DatabaseId,
	id: BackupId,
) -> AppResult<BackupId> {
	authorize(&s.db, user, team, Action::Delete, Resource::Backup).await?;
	let database = scope::database(&s.db, team, project, database).await?;
	let backup = backups::find(&s.db, database.id, id)
		.await?
		.ok_or(ServerError::BackupNotFound)?;

	if matches!(backup.status, BackupStatus::Queued | BackupStatus::Running) {
		return Err(ServerError::Conflict("backup is in progress".to_owned()));
	}
	backups::purge(s, &backup).await?;
	Ok(backup.id)
}
