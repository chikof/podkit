//! Volume backup use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or queue a job.

use time::OffsetDateTime;

use super::subject::{self, Owner, Subject, Vol};
use super::{
	Consistency, DEFAULT_KEEP, MAX_KEEP, Policy, Restore, RestoreMode, RestoreState, VolumeBackup,
	advance_restore,
};
use crate::auth::permission::{Action, Resource, authorize};
use crate::auth::users;
use crate::backups::{self, BackupStatus, next_run, parse_schedule};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{BackupTargetId, ProjectId, TeamId, UserId, VolumeBackupId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;
use crate::{scope, validate, volumes};

pub const DEFAULT_PAGE_SIZE: i64 = 20;
pub const MAX_PAGE_SIZE: i64 = 200;
/// Restores a volume's section lists.
const RECENT_RESTORES: i64 = 3;
/// Runs the schedule preview lists.
pub const PREVIEW_RUNS: usize = 3;

/// The settings form of a volume's backups.
#[derive(Debug, Clone, Default)]
pub struct SettingsInput {
	pub target_id: Option<BackupTargetId>,
	/// Five-field UTC cron; empty or `None` for manual backups only.
	pub schedule: Option<String>,
	/// Backups kept; the default when `None`.
	pub keep: Option<i32>,
	/// Live when `None`.
	pub consistency: Option<Consistency>,
}

/// One backup as the page lists it, with the name of who started it.
#[derive(Debug, Clone)]
pub struct Entry {
	pub backup: VolumeBackup,
	/// `None` for a scheduled backup, or when the user no longer exists.
	pub triggered_by: Option<String>,
}

/// A volume's backup section: its settings and history.
#[derive(Debug, Clone)]
pub struct View {
	pub subject: Subject,
	/// `None` until the volume was configured or backed up once.
	pub policy: Option<Policy>,
	pub target_name: Option<String>,
	pub entries: Vec<Entry>,
	pub restores: Vec<Restore>,
	/// The next runs of the schedule.
	pub next_runs: Vec<OffsetDateTime>,
	/// Whether a backup can start now, or why not.
	pub can_start: Result<(), String>,
}

/// The restore form.
#[derive(Debug, Clone)]
pub struct RestoreInput {
	pub mode: RestoreMode,
	/// The new volume: a mount path for an application, a compose volume name for a
	/// stack. Needed, and only used, for [`RestoreMode::New`].
	pub new_volume: Option<String>,
	/// The volume's mount path or compose name, typed by the user.
	pub confirm: String,
}

fn validation(message: &str) -> ServerError {
	ServerError::Validation(message.to_owned())
}

/// The next `count` runs of `schedule`.
///
/// # Errors
/// The parser's message when `schedule` is not a valid 5-field cron expression.
pub fn upcoming_runs(schedule: &str, count: usize) -> Result<Vec<OffsetDateTime>, String> {
	crate::databases::service::upcoming_runs(schedule, count)
}

/// What the page needs for `subject`. Caller must hold `read backup` on the team.
async fn view_of(s: &AppState, user: UserId, team: TeamId, subject: Subject) -> AppResult<View> {
	let policy = super::policy_by_name(&s.db, &subject.volume_name).await?;
	let Some(policy) = policy else {
		return Ok(View {
			subject,
			policy: None,
			target_name: None,
			entries: Vec::new(),
			restores: Vec::new(),
			next_runs: Vec::new(),
			can_start: Err("choose a destination first".to_owned()),
		});
	};

	let rows = super::list_backups(&s.db, policy.id, None, DEFAULT_PAGE_SIZE).await?;
	let mut names: Vec<(UserId, Option<String>)> = Vec::new();
	let mut entries = Vec::with_capacity(rows.len());
	for backup in rows {
		let triggered_by = match backup.triggered_by {
			None => None,
			Some(id) => {
				if let Some((_, name)) = names.iter().find(|(known, _)| *known == id) {
					name.clone()
				} else {
					let name = users::name_of(&s.db, id).await?;
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
	let restores = super::recent_restores(&s.db, policy.id, RECENT_RESTORES).await?;

	let target_name = match policy.target_id {
		Some(id) => scope::backup_target(&s.db, team, id)
			.await
			.ok()
			.map(|t| t.name),
		None => None,
	};
	let next_runs = policy
		.schedule
		.as_deref()
		.and_then(|schedule| upcoming_runs(schedule, PREVIEW_RUNS).ok())
		.unwrap_or_default();
	let can_start = match (&target_name, super::busy(&s.db, policy.id).await?) {
		(None, _) => Err("choose a destination first".to_owned()),
		(Some(_), true) => Err("a backup or restore of this volume is running".to_owned()),
		(Some(_), false) => Ok(()),
	};
	let _ = user;
	Ok(View {
		subject,
		policy: Some(policy),
		target_name,
		entries,
		restores,
		next_runs,
		can_start,
	})
}

/// One volume's section. Caller must hold `read backup` on the team.
pub async fn overview(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
) -> AppResult<View> {
	authorize(&s.db, user, team, Action::Read, Resource::Backup).await?;
	let subject = subject::resolve(s, team, project, vol).await?;
	view_of(s, user, team, subject).await
}

/// The sections of every volume of an application. Caller must hold `read backup`.
pub async fn overviews_of_application(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: crate::ids::ApplicationId,
) -> AppResult<Vec<View>> {
	authorize(&s.db, user, team, Action::Read, Resource::Backup).await?;
	let subjects = subject::of_application(&s.db, team, project, app).await?;
	let mut views = Vec::with_capacity(subjects.len());
	for subject in subjects {
		views.push(view_of(s, user, team, subject).await?);
	}
	Ok(views)
}

/// The sections of every volume of a stack. Caller must hold `read backup`.
pub async fn overviews_of_stack(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	stack: crate::ids::StackId,
) -> AppResult<Vec<View>> {
	authorize(&s.db, user, team, Action::Read, Resource::Backup).await?;
	let subjects = subject::of_stack(s, team, project, stack).await?;
	let mut views = Vec::with_capacity(subjects.len());
	for subject in subjects {
		views.push(view_of(s, user, team, subject).await?);
	}
	Ok(views)
}

/// The next runs of `schedule`, for the preview under the field. Caller must hold
/// `update backup` on the team.
pub async fn preview_schedule(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
	schedule: &str,
) -> AppResult<Result<Vec<OffsetDateTime>, String>> {
	authorize(&s.db, user, team, Action::Update, Resource::Backup).await?;
	subject::resolve(s, team, project, vol).await?;
	Ok(upcoming_runs(schedule, PREVIEW_RUNS))
}

/// Stores the volume's destination, schedule, retention and consistency. Caller must
/// hold `update backup` on the team; the destination must be the team's own.
pub async fn save_settings(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
	input: SettingsInput,
) -> AppResult<Policy> {
	authorize(&s.db, user, team, Action::Update, Resource::Backup).await?;
	let subject = subject::resolve(s, team, project, vol).await?;

	let keep = input.keep.unwrap_or(DEFAULT_KEEP);
	if !(1..=MAX_KEEP).contains(&keep) {
		return Err(validation("keep must be between 1 and 365"));
	}
	if let Some(target) = input.target_id {
		scope::backup_target(&s.db, team, target).await?;
	}
	let schedule = input
		.schedule
		.as_deref()
		.map(str::trim)
		.filter(|s| !s.is_empty());
	let next_run_at = match schedule {
		None => None,
		Some(schedule) => {
			parse_schedule(schedule)
				.map_err(|e| ServerError::Validation(format!("not a valid schedule: {e}")))?;
			if input.target_id.is_none() {
				return Err(validation("choose a destination to run a schedule"));
			}
			Some(
				next_run(schedule, OffsetDateTime::now_utc())
					.map_err(|e| ServerError::Validation(format!("not a valid schedule: {e}")))?,
			)
		}
	};

	let policy = super::ensure_policy(&s.db, &subject).await?;
	Ok(super::save_policy(
		&s.db,
		policy.id,
		input.target_id,
		schedule,
		next_run_at,
		keep,
		input.consistency.unwrap_or(Consistency::Live),
	)
	.await?)
}

/// Queues a backup of the volume to its destination and returns immediately. Caller
/// must hold `create backup` on the team.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
) -> AppResult<VolumeBackup> {
	authorize(&s.db, user, team, Action::Create, Resource::Backup).await?;
	let subject = subject::resolve(s, team, project, vol).await?;
	let policy = super::ensure_policy(&s.db, &subject).await?;
	let target = policy
		.target_id
		.ok_or_else(|| validation("configure a backup destination first"))?;
	if super::busy(&s.db, policy.id).await? {
		return Err(ServerError::Conflict(
			"a backup or restore of this volume is already running".to_owned(),
		));
	}
	Ok(super::queue_backup(s, &subject, &policy, target, Some(user)).await?)
}

/// The volume's backups, newest first, paged with `before` and `limit` (1..=200).
/// Caller must hold `read backup` on the team.
pub async fn list(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
	before: Option<VolumeBackupId>,
	limit: Option<i64>,
) -> AppResult<Vec<VolumeBackup>> {
	authorize(&s.db, user, team, Action::Read, Resource::Backup).await?;
	let subject = subject::resolve(s, team, project, vol).await?;
	let Some(policy) = super::policy_by_name(&s.db, &subject.volume_name).await? else {
		return Ok(Vec::new());
	};
	let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
	Ok(super::list_backups(&s.db, policy.id, before, limit).await?)
}

/// Deletes the backup's object from its destination, then the row. A backup that is
/// still queued or running cannot be deleted. Caller must hold `delete backup`.
pub async fn remove(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
	id: VolumeBackupId,
) -> AppResult<VolumeBackupId> {
	authorize(&s.db, user, team, Action::Delete, Resource::Backup).await?;
	let subject = subject::resolve(s, team, project, vol).await?;
	let backup = backup_of(s, &subject, id).await?;
	if matches!(backup.status, BackupStatus::Queued | BackupStatus::Running) {
		return Err(ServerError::Conflict("backup is in progress".to_owned()));
	}
	super::run::purge(s, &backup).await?;
	Ok(backup.id)
}

async fn backup_of(s: &AppState, subject: &Subject, id: VolumeBackupId) -> AppResult<VolumeBackup> {
	let policy = super::policy_by_name(&s.db, &subject.volume_name)
		.await?
		.ok_or(ServerError::BackupNotFound)?;
	super::find_backup(&s.db, policy.id, id)
		.await?
		.ok_or(ServerError::BackupNotFound)
}

/// Queues a restore of a succeeded backup. Nothing is queued unless `input.confirm`
/// is exactly the volume's mount path or compose name: the check lives here so a
/// forged request cannot skip it. The owner is stopped while the archive is written
/// and started again after. Caller must hold `update volume` on the team.
pub async fn restore(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	vol: &Vol,
	id: VolumeBackupId,
	input: RestoreInput,
) -> AppResult<Restore> {
	authorize(&s.db, user, team, Action::Update, Resource::Volume).await?;
	let subject = subject::resolve(s, team, project, vol).await?;
	if input.confirm != subject.label {
		return Err(validation(
			"The name doesn't match. Type the volume's path or name exactly to restore.",
		));
	}
	let backup = backup_of(s, &subject, id).await?;
	if backup.status != BackupStatus::Succeeded || backup.object_key.is_none() {
		return Err(validation("only a succeeded backup can be restored"));
	}
	if backup.target_id.is_none() {
		return Err(validation(
			"the backup's destination was deleted, so it can't be restored",
		));
	}
	if super::busy(&s.db, backup.policy_id).await? {
		return Err(ServerError::Conflict(
			"a backup or restore of this volume is already running".to_owned(),
		));
	}

	let mut tx = s.db.begin().await?;
	let dest = match input.mode {
		RestoreMode::Same => subject.volume_name.clone(),
		RestoreMode::New => {
			let new = input
				.new_volume
				.as_deref()
				.map(str::trim)
				.unwrap_or_default();
			match subject.owner {
				Owner::App(app) => {
					if !validate::abs_path(new) {
						return Err(validation(
							"the new volume's mount path must be an absolute path other than /",
						));
					}
					volumes::insert(&mut *tx, app, new)
						.await
						.map_err(|e| {
							if is_unique_violation(&e, volumes::MOUNT_PATH_CONSTRAINT) {
								ServerError::Conflict(
									"a volume is already mounted at that path".to_owned(),
								)
							} else {
								e.into()
							}
						})?
						.name
				}
				Owner::Stack(stack) => {
					if !subject::valid_volume_key(new) {
						return Err(validation(
							"a volume name uses letters, digits, '.', '_' and '-', and starts with a letter or digit",
						));
					}
					let known = scope::stack(&s.db, team, project, stack).await?;
					if subject::stack_volume_keys(s, &known)
						.iter()
						.any(|k| k == new)
					{
						return Err(validation(
							"the stack already has a volume with that name; choose a new one",
						));
					}
					crate::stacks::translate::volume_name(stack.0, new)
				}
			}
		}
	};
	let restore = super::insert_restore(&mut *tx, backup.id, input.mode, &dest, user).await?;
	tx.commit().await?;

	if let Err(e) = jobs::enqueue(
		s,
		JobKind::VolumeRestore,
		subject.server,
		restore.id.0,
		subject.lock_key(),
	)
	.await
	{
		if let Err(mark) = advance_restore(
			&s.db,
			restore.id,
			RestoreState::Queued,
			RestoreState::Failed,
			Some("failed to queue the restore job"),
		)
		.await
		{
			tracing::warn!(
				restore = restore.id.0,
				"failed to mark unqueued restore failed: {mark}"
			);
		}
		return Err(e.into());
	}
	Ok(restore)
}

/// The names of the team's backup destinations, for the settings form. Caller must
/// hold `read backup` on the team.
pub async fn destinations(
	s: &AppState,
	user: UserId,
	team: TeamId,
) -> AppResult<Vec<(BackupTargetId, String)>> {
	authorize(&s.db, user, team, Action::Read, Resource::Backup).await?;
	Ok(backups::targets::list_for_team(&s.db, team)
		.await?
		.into_iter()
		.map(|t| (t.id, t.name))
		.collect())
}
