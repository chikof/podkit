//! Backups of application and stack volumes (migration 0054).
//!
//! A volume's backup settings are a [`Policy`]: the destination, the schedule, how
//! many backups to keep and the [`Consistency`]. A [`VolumeBackup`] is one archive of
//! the volume in that destination. A [`Restore`] writes one back, into the same
//! volume or a new one, and walks the [`RestoreState`] machine while it does.
//!
//! The control plane never touches the bytes. A job asks the server's agent to
//! archive the volume to a gzip file (`run`), makes the S3 multipart upload and hands
//! the agent a presigned url per part; a restore is the same road in reverse.

mod routes;
pub mod run;
pub mod schedule;
pub mod service;
pub mod subject;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use sqlx::postgres::PgQueryResult;
use time::OffsetDateTime;

use crate::backups::BackupStatus;
use crate::ids::{
	BackupTargetId, StackId, TeamId, UserId, VolumeBackupId, VolumeId, VolumePolicyId,
	VolumeRestoreId,
};
use crate::jobs::{self, JobFailure, JobKind};
use crate::state::AppState;

pub use routes::{VolRoute, app_router, stack_router};

/// Backups kept when a policy does not say.
pub const DEFAULT_KEEP: i32 = 7;
/// Most backups a policy may keep.
pub const MAX_KEEP: i32 = 365;

/// Smallest part of a multipart upload that is not the last: S3 refuses less than 5 MiB.
/// <https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html>
const MIN_PART_BYTES: u64 = 64 << 20;
/// An upload has at most this many parts.
const MAX_PARTS: u64 = 10_000;

/// Whether a volume's owner keeps running while the volume is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Consistency {
	/// Read the volume as it is. Files written during the read may not agree with
	/// each other; the backup says so when something was running.
	Live,
	/// Stop the owner for the read, then start it again. The archive is consistent.
	Stop,
}

impl Consistency {
	/// The stored and API form.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Live => "live",
			Self::Stop => "stop",
		}
	}

	/// Reads the stored and API form.
	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"live" => Some(Self::Live),
			"stop" => Some(Self::Stop),
			_ => None,
		}
	}
}

/// Where a restore is, in order. `Succeeded`, `Failed` and `Cancelled` end it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum RestoreState {
	/// Waiting for its job.
	Queued,
	/// Stopping what uses the volume.
	Stopping,
	/// The agent is writing the archive into the volume.
	Restoring,
	/// Starting what was stopped.
	Starting,
	/// The volume holds the backup and what was stopped runs again.
	Succeeded,
	/// Something went wrong; `error` says what.
	Failed,
	/// Cancelled before it finished.
	Cancelled,
}

impl RestoreState {
	/// Whether the restore is over.
	#[must_use]
	pub const fn is_final(self) -> bool {
		matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
	}

	/// Whether a restore in this state may move to `next`. The happy path runs in
	/// order; any state that is not final can fail or be cancelled; nothing leaves a
	/// final state.
	#[must_use]
	pub const fn can_transition_to(self, next: Self) -> bool {
		use RestoreState::{Cancelled, Failed, Queued, Restoring, Starting, Stopping, Succeeded};
		matches!(
			(self, next),
			(Queued, Stopping)
				| (Stopping, Restoring)
				| (Restoring, Starting)
				| (Starting, Succeeded)
		) || (!self.is_final() && matches!(next, Failed | Cancelled))
	}

	/// What the page says.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Queued => "Queued",
			Self::Stopping => "Stopping",
			Self::Restoring => "Restoring",
			Self::Starting => "Starting",
			Self::Succeeded => "Restored",
			Self::Failed => "Failed",
			Self::Cancelled => "Cancelled",
		}
	}
}

/// Where a restore writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum RestoreMode {
	/// Into the backed-up volume, which is emptied first.
	Same,
	/// Into another volume, which nothing else uses yet.
	New,
}

/// Row from `volume_backup_policies`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Policy {
	pub id: VolumePolicyId,
	pub volume_id: Option<VolumeId>,
	pub stack_id: Option<StackId>,
	/// Mount path or compose name.
	pub label: String,
	pub target_id: Option<BackupTargetId>,
	pub schedule: Option<String>,
	pub next_run_at: Option<OffsetDateTime>,
	pub keep: i32,
	pub consistency: Consistency,
}

pub(crate) const POLICY_COLUMNS: &str = "id, volume_id, stack_id, label, target_id, schedule,
	next_run_at, keep, consistency";

/// Row from `volume_backups`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VolumeBackup {
	pub id: VolumeBackupId,
	pub policy_id: VolumePolicyId,
	/// `None` once the destination was deleted; the object is then unreachable.
	pub target_id: Option<BackupTargetId>,
	pub status: BackupStatus,
	pub consistency: Consistency,
	pub object_key: Option<String>,
	pub size_bytes: Option<i64>,
	pub raw_bytes: Option<i64>,
	pub entries: Option<i64>,
	pub sha256: Option<String>,
	pub warning: Option<String>,
	pub error: Option<String>,
	/// `None` for scheduled backups.
	pub triggered_by: Option<UserId>,
	pub created_at: OffsetDateTime,
	pub finished_at: Option<OffsetDateTime>,
}

const BACKUP_COLUMNS: &str = "id, policy_id, target_id, status, consistency, object_key,
	size_bytes, raw_bytes, entries, sha256, warning, error, triggered_by, created_at, finished_at";

/// Row from `volume_restores`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Restore {
	pub id: VolumeRestoreId,
	pub backup_id: VolumeBackupId,
	pub status: RestoreState,
	pub mode: RestoreMode,
	/// Podman name of the volume written to.
	pub dest_volume: String,
	pub error: Option<String>,
	pub created_at: OffsetDateTime,
}

const RESTORE_COLUMNS: &str = "id, backup_id, status, mode, dest_volume, error, created_at";

/// `{prefix}{team}/volumes/{policy}/{backup}.tar.gz`, where a non-empty prefix gets
/// a trailing `/`.
#[must_use]
pub fn object_key(
	prefix: &str,
	team: TeamId,
	policy: VolumePolicyId,
	backup: VolumeBackupId,
) -> String {
	let sep = if prefix.is_empty() || prefix.ends_with('/') {
		""
	} else {
		"/"
	};
	format!("{prefix}{sep}{team}/volumes/{policy}/{backup}.tar.gz")
}

/// How a `size`-byte archive is cut for a multipart upload: the size of every part
/// but the last, and the number of parts. The part size grows with the archive so
/// the count stays under the 10 000 parts S3 allows.
#[must_use]
pub fn plan_parts(size: u64) -> (u64, u16) {
	const MIB: u64 = 1 << 20;
	let wanted = size.div_ceil(MAX_PARTS - 1).div_ceil(MIB) * MIB;
	let part = wanted.max(MIN_PART_BYTES);
	let count = size.div_ceil(part).max(1);
	(part, u16::try_from(count).unwrap_or(u16::MAX))
}

// --- policies ----------------------------------------------------------------------------

pub(crate) async fn policy_by_name(
	db: impl PgExecutor<'_>,
	volume_name: &str,
) -> Result<Option<Policy>, sqlx::Error> {
	sqlx::query_as::<_, Policy>(sqlx::AssertSqlSafe(format!(
		"SELECT {POLICY_COLUMNS} FROM volume_backup_policies WHERE volume_name = $1"
	)))
	.bind(volume_name)
	.fetch_optional(db)
	.await
}

pub(crate) async fn policy_by_id(
	db: impl PgExecutor<'_>,
	id: VolumePolicyId,
) -> Result<Option<Policy>, sqlx::Error> {
	sqlx::query_as::<_, Policy>(sqlx::AssertSqlSafe(format!(
		"SELECT {POLICY_COLUMNS} FROM volume_backup_policies WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// The policy of the volume, created with the defaults the first time.
pub(crate) async fn ensure_policy(
	db: impl PgExecutor<'_>,
	subject: &subject::Subject,
) -> Result<Policy, sqlx::Error> {
	sqlx::query_as::<_, Policy>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO volume_backup_policies (id, volume_id, stack_id, volume_name, label)
		VALUES ($1, $2, $3, $4, $5)
		ON CONFLICT (volume_name) DO UPDATE SET label = EXCLUDED.label
		RETURNING {POLICY_COLUMNS}"
	)))
	.bind(VolumePolicyId::generate())
	.bind(subject.volume_id)
	.bind(subject.stack_id)
	.bind(&subject.volume_name)
	.bind(&subject.label)
	.fetch_one(db)
	.await
}

/// Replaces the policy's settings. `next_run_at` is when the schedule fires next;
/// `None` with no schedule.
pub(crate) async fn save_policy(
	db: impl PgExecutor<'_>,
	id: VolumePolicyId,
	target: Option<BackupTargetId>,
	schedule: Option<&str>,
	next_run_at: Option<OffsetDateTime>,
	keep: i32,
	consistency: Consistency,
) -> Result<Policy, sqlx::Error> {
	sqlx::query_as::<_, Policy>(sqlx::AssertSqlSafe(format!(
		"UPDATE volume_backup_policies
		SET target_id = $2, schedule = $3, next_run_at = $4, keep = $5, consistency = $6
		WHERE id = $1
		RETURNING {POLICY_COLUMNS}"
	)))
	.bind(id)
	.bind(target)
	.bind(schedule)
	.bind(next_run_at)
	.bind(keep)
	.bind(consistency)
	.fetch_one(db)
	.await
}

// --- backups -----------------------------------------------------------------------------

async fn insert_backup(
	db: impl PgExecutor<'_>,
	policy: &Policy,
	target: BackupTargetId,
	triggered_by: Option<UserId>,
) -> Result<VolumeBackup, sqlx::Error> {
	sqlx::query_as::<_, VolumeBackup>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO volume_backups (id, policy_id, target_id, consistency, triggered_by)
		VALUES ($1, $2, $3, $4, $5)
		RETURNING {BACKUP_COLUMNS}"
	)))
	.bind(VolumeBackupId::generate())
	.bind(policy.id)
	.bind(target)
	.bind(policy.consistency)
	.bind(triggered_by)
	.fetch_one(db)
	.await
}

pub(crate) async fn backup_by_id(
	db: impl PgExecutor<'_>,
	id: VolumeBackupId,
) -> Result<Option<VolumeBackup>, sqlx::Error> {
	sqlx::query_as::<_, VolumeBackup>(sqlx::AssertSqlSafe(format!(
		"SELECT {BACKUP_COLUMNS} FROM volume_backups WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// A backup of `policy`; the id of another volume's backup never resolves.
pub(crate) async fn find_backup(
	db: impl PgExecutor<'_>,
	policy: VolumePolicyId,
	id: VolumeBackupId,
) -> Result<Option<VolumeBackup>, sqlx::Error> {
	sqlx::query_as::<_, VolumeBackup>(sqlx::AssertSqlSafe(format!(
		"SELECT {BACKUP_COLUMNS} FROM volume_backups WHERE id = $2 AND policy_id = $1"
	)))
	.bind(policy)
	.bind(id)
	.fetch_optional(db)
	.await
}

/// The policy's backups, newest first; `before` is the keyset cursor.
pub(crate) async fn list_backups(
	db: impl PgExecutor<'_>,
	policy: VolumePolicyId,
	before: Option<VolumeBackupId>,
	limit: i64,
) -> Result<Vec<VolumeBackup>, sqlx::Error> {
	sqlx::query_as::<_, VolumeBackup>(sqlx::AssertSqlSafe(format!(
		"SELECT {BACKUP_COLUMNS} FROM volume_backups
		WHERE policy_id = $1 AND ($2::BIGINT IS NULL OR id < $2)
		ORDER BY id DESC LIMIT $3"
	)))
	.bind(policy)
	.bind(before.map(|b| b.0))
	.bind(limit)
	.fetch_all(db)
	.await
}

/// Succeeded backups beyond the newest `keep`.
pub(crate) async fn beyond_retention(
	db: impl PgExecutor<'_>,
	policy: VolumePolicyId,
	keep: i64,
) -> Result<Vec<VolumeBackup>, sqlx::Error> {
	sqlx::query_as::<_, VolumeBackup>(sqlx::AssertSqlSafe(format!(
		"SELECT {BACKUP_COLUMNS} FROM volume_backups
		WHERE policy_id = $1 AND status = 'succeeded'
		ORDER BY id DESC OFFSET $2"
	)))
	.bind(policy)
	.bind(keep)
	.fetch_all(db)
	.await
}

/// Whether a backup or a restore of the policy's volume is queued or running.
pub(crate) async fn busy(
	db: impl PgExecutor<'_>,
	policy: VolumePolicyId,
) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM volume_backups
			WHERE policy_id = $1 AND status IN ('queued', 'running'))
		OR EXISTS (SELECT 1 FROM volume_restores r JOIN volume_backups b ON b.id = r.backup_id
			WHERE b.policy_id = $1 AND r.status IN ('queued', 'stopping', 'restoring', 'starting'))",
	)
	.bind(policy)
	.fetch_one(db)
	.await
}

async fn mark_running(db: impl PgExecutor<'_>, id: VolumeBackupId) -> Result<(), sqlx::Error> {
	sqlx::query("UPDATE volume_backups SET status = 'running' WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

/// What a finished archive recorded.
pub(crate) struct Stored<'a> {
	pub key: &'a str,
	pub size: i64,
	pub raw_bytes: i64,
	pub entries: i64,
	pub sha256: &'a str,
	pub warning: Option<&'a str>,
}

async fn mark_succeeded(
	db: impl PgExecutor<'_>,
	id: VolumeBackupId,
	stored: &Stored<'_>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE volume_backups
		SET status = 'succeeded', object_key = $2, size_bytes = $3, raw_bytes = $4, entries = $5,
			sha256 = $6, warning = $7, finished_at = NOW()
		WHERE id = $1",
	)
	.bind(id)
	.bind(stored.key)
	.bind(stored.size)
	.bind(stored.raw_bytes)
	.bind(stored.entries)
	.bind(stored.sha256)
	.bind(stored.warning)
	.execute(db)
	.await?;
	Ok(())
}

pub(crate) async fn mark_failed(
	db: impl PgExecutor<'_>,
	id: VolumeBackupId,
	error: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE volume_backups SET status = 'failed', error = $2, finished_at = NOW()
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(error)
	.execute(db)
	.await?;
	Ok(())
}

pub(crate) async fn delete_backup_row(
	db: impl PgExecutor<'_>,
	id: VolumeBackupId,
) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM volume_backups WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

/// Inserts a queued backup of the volume and enqueues its job: the one entry point
/// of the manual route and of the scheduler.
///
/// # Errors
/// A database error; a backup whose job could not be enqueued is marked failed first.
pub async fn queue_backup(
	state: &AppState,
	subject: &subject::Subject,
	policy: &Policy,
	target: BackupTargetId,
	triggered_by: Option<UserId>,
) -> Result<VolumeBackup, sqlx::Error> {
	let backup = insert_backup(&state.db, policy, target, triggered_by).await?;
	if let Err(e) = jobs::enqueue(
		state,
		JobKind::VolumeBackup,
		subject.server,
		backup.id.0,
		subject.lock_key(),
	)
	.await
	{
		if let Err(mark) = mark_failed(&state.db, backup.id, "failed to queue backup job").await {
			tracing::warn!(
				backup = backup.id.0,
				"failed to mark unqueued backup failed: {mark}"
			);
		}
		return Err(e);
	}
	Ok(backup)
}

// --- restores ----------------------------------------------------------------------------

pub(crate) async fn insert_restore(
	db: impl PgExecutor<'_>,
	backup: VolumeBackupId,
	mode: RestoreMode,
	dest_volume: &str,
	triggered_by: UserId,
) -> Result<Restore, sqlx::Error> {
	sqlx::query_as::<_, Restore>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO volume_restores (id, backup_id, mode, dest_volume, triggered_by)
		VALUES ($1, $2, $3, $4, $5)
		RETURNING {RESTORE_COLUMNS}"
	)))
	.bind(VolumeRestoreId::generate())
	.bind(backup)
	.bind(mode)
	.bind(dest_volume)
	.bind(triggered_by)
	.fetch_one(db)
	.await
}

pub(crate) async fn restore_by_id(
	db: impl PgExecutor<'_>,
	id: VolumeRestoreId,
) -> Result<Option<Restore>, sqlx::Error> {
	sqlx::query_as::<_, Restore>(sqlx::AssertSqlSafe(format!(
		"SELECT {RESTORE_COLUMNS} FROM volume_restores WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// The newest restores of the policy's backups.
pub(crate) async fn recent_restores(
	db: impl PgExecutor<'_>,
	policy: VolumePolicyId,
	limit: i64,
) -> Result<Vec<Restore>, sqlx::Error> {
	sqlx::query_as::<_, Restore>(sqlx::AssertSqlSafe(format!(
		"SELECT {} FROM volume_restores r JOIN volume_backups b ON b.id = r.backup_id
		WHERE b.policy_id = $1 ORDER BY r.id DESC LIMIT $2",
		RESTORE_COLUMNS
			.split(',')
			.map(|c| format!("r.{}", c.trim()))
			.collect::<Vec<_>>()
			.join(", ")
	)))
	.bind(policy)
	.bind(limit)
	.fetch_all(db)
	.await
}

/// Why a restore cannot move.
#[derive(Debug, thiserror::Error)]
pub enum AdvanceError {
	#[error("a restore cannot go from {from:?} to {to:?}")]
	Order {
		from: RestoreState,
		to: RestoreState,
	},
	#[error("the restore is no longer {0:?}")]
	Moved(RestoreState),
	#[error(transparent)]
	Db(#[from] sqlx::Error),
}

/// Moves the restore from `from` to `to`, only when [`RestoreState::can_transition_to`]
/// allows it and the row is still in `from`. A final state stamps `finished_at`.
///
/// # Errors
/// [`AdvanceError::Order`] for a move the machine forbids, [`AdvanceError::Moved`]
/// when something else moved the row first.
pub async fn advance_restore(
	db: impl PgExecutor<'_>,
	id: VolumeRestoreId,
	from: RestoreState,
	to: RestoreState,
	error: Option<&str>,
) -> Result<(), AdvanceError> {
	if !from.can_transition_to(to) {
		return Err(AdvanceError::Order { from, to });
	}
	let done = sqlx::query(
		"UPDATE volume_restores
		SET status = $3, error = COALESCE($4, error),
			finished_at = CASE WHEN $5 THEN NOW() ELSE finished_at END
		WHERE id = $1 AND status = $2",
	)
	.bind(id)
	.bind(from)
	.bind(to)
	.bind(error)
	.bind(to.is_final())
	.execute(db)
	.await?;
	if done.rows_affected() == 0 {
		return Err(AdvanceError::Moved(from));
	}
	Ok(())
}

// --- failures of jobs ---------------------------------------------------------------------

/// Reflects a failed or cancelled backup job on its row.
pub async fn fail_backup(
	state: &AppState,
	id: i64,
	failure: &JobFailure,
) -> Result<PgQueryResult, sqlx::Error> {
	let message = match failure {
		JobFailure::Cancelled => "cancelled",
		JobFailure::Failed(m) => m.as_str(),
	};
	// The status guard keeps a finished backup from being rewritten.
	sqlx::query(
		"UPDATE volume_backups SET status = 'failed', error = $2, finished_at = NOW()
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(message)
	.execute(&state.db)
	.await
}

/// Reflects a failed or cancelled restore job on its row, from whatever state it was in.
pub async fn fail_restore(
	state: &AppState,
	id: i64,
	failure: &JobFailure,
) -> Result<PgQueryResult, sqlx::Error> {
	let (status, message) = match failure {
		JobFailure::Cancelled => (RestoreState::Cancelled, "cancelled"),
		JobFailure::Failed(m) => (RestoreState::Failed, m.as_str()),
	};
	sqlx::query(
		"UPDATE volume_restores SET status = $2, error = $3, finished_at = NOW()
		WHERE id = $1 AND status IN ('queued', 'stopping', 'restoring', 'starting')",
	)
	.bind(id)
	.bind(status)
	.bind(message)
	.execute(&state.db)
	.await
}
