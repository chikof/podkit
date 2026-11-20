//! The Environment clone job.
//!
//! The configuration was copied when the clone was requested; this brings the clone to
//! life, one step at a time, and writes each step's outcome to the clone's report as it
//! goes. A step that fails does not stop the others: the environment is consistent
//! (every resource exists, the failed ones are simply not running or not filled), and the
//! report says which.
//!
//! The steps queue the jobs that already do the work (provisioning a database, a backup,
//! a restore, a deploy) and wait for the ones whose result the next step needs.
//! Data goes through the team's backup destination chosen for the clone: a database is
//! dumped and the dump restored into the clone, a volume is archived and the archive
//! restored into the clone's new volume. The dumps stay in the destination as ordinary
//! backups of the source.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use proto::Stream as LogStream;
use tokio_util::sync::CancellationToken;

use super::{CloneStatus, EnvironmentClone, Kind, Step, StepStatus, find_by_id, save_report};
use crate::applications::{self, Application};
use crate::backups::{self, BackupStatus};
use crate::databases::{self, Database, DatabaseStatus};
use crate::deployments;
use crate::error::AppResult;
use crate::ids::{ApplicationId, BackupTargetId, CloneId, DatabaseId, ModelId, StackId, UserId};
use crate::jobs::{self, Job, JobFailure, JobKind, JobStatus};
use crate::logs::JobLog;
use crate::models::{self, Model};
use crate::stacks::{self, Stack, translate};
use crate::state::AppState;
use crate::volume_backups::subject::{self, Vol};
use crate::volume_backups::{self, RestoreMode, RestoreState};
use crate::volumes;

/// How long one step may wait for the jobs it queued.
const STEP_LIMIT: Duration = Duration::from_hours(1);
const POLL: Duration = Duration::from_secs(2);

/// One thing the clone does, with what it needs to do it.
#[derive(Debug, Clone)]
pub enum Work {
	/// Start the cloned database (provision it).
	Provision(Database),
	/// Dump the source database and restore the dump into the clone.
	CopyDatabase {
		source: Database,
		clone: Database,
	},
	/// Archive a source volume and restore it into the clone's volume `dest`.
	CopyVolume {
		vol: Vol,
		dest: String,
		label: String,
		owner: String,
	},
	DeployApp(Application),
	DeployStack(Stack),
	StartModel(Model),
}

impl Work {
	/// The step as the report first shows it.
	#[must_use]
	pub fn step(&self) -> Step {
		Step::pending(match self {
			Self::Provision(d) => format!("Start database {}", d.name),
			Self::CopyDatabase { clone, .. } => format!("Copy the data of {}", clone.name),
			Self::CopyVolume { label, owner, .. } => format!("Copy volume {label} of {owner}"),
			Self::DeployApp(a) => format!("Deploy {}", a.name),
			Self::DeployStack(s) => format!("Deploy stack {}", s.name),
			Self::StartModel(m) => format!("Start model {}", m.name),
		})
	}
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("environment clone database error: {e}");
	JobFailure::Failed("database error".to_owned())
}

fn failed(e: impl std::fmt::Display) -> JobFailure {
	JobFailure::Failed(e.to_string())
}

/// The steps of `clone`, in the order they run: databases first, then data, then deploys.
///
/// # Errors
/// The database fails.
pub async fn work_list(s: &AppState, clone: &EnvironmentClone) -> AppResult<Vec<Work>> {
	let options = &clone.options.0;
	let mut work = Vec::new();
	let mut database_pairs = Vec::new();
	let (mut apps, mut stack_list, mut model_list) = (Vec::new(), Vec::new(), Vec::new());
	for p in &clone.pairs.0 {
		match p.kind {
			Kind::Application => {
				if let (Some(old), Some(new)) = (
					applications::find_by_id(&s.db, ApplicationId(p.source)).await?,
					applications::find_by_id(&s.db, ApplicationId(p.id)).await?,
				) {
					apps.push((old, new));
				}
			}
			Kind::Database => {
				if let (Some(old), Some(new)) = (
					databases::find_by_id(&s.db, DatabaseId(p.source)).await?,
					databases::find_by_id(&s.db, DatabaseId(p.id)).await?,
				) {
					database_pairs.push((old, new));
				}
			}
			Kind::Stack => {
				if let (Some(old), Some(new)) = (
					stacks::find_by_id(&s.db, StackId(p.source)).await?,
					stacks::find_by_id(&s.db, StackId(p.id)).await?,
				) {
					stack_list.push((old, new));
				}
			}
			Kind::Model => {
				if let Some(new) = models::find_by_id(&s.db, ModelId(p.id)).await? {
					model_list.push(new);
				}
			}
		}
	}

	for (_, new) in &database_pairs {
		work.push(Work::Provision(new.clone()));
	}
	if options.data {
		for (old, new) in &database_pairs {
			if old.engine().dump_ext().is_some() {
				work.push(Work::CopyDatabase {
					source: old.clone(),
					clone: new.clone(),
				});
			}
		}
		for (old, new) in &apps {
			let new_volumes = volumes::list(&s.db, new.id).await?;
			for v in volumes::list(&s.db, old.id).await? {
				if let Some(dest) = new_volumes.iter().find(|n| n.mount_path == v.mount_path) {
					work.push(Work::CopyVolume {
						vol: Vol::App {
							app: old.id,
							volume: v.id,
						},
						dest: dest.name.clone(),
						label: v.mount_path,
						owner: new.name.clone(),
					});
				}
			}
		}
		for (old, new) in &stack_list {
			for key in subject::stack_volume_keys(s, new) {
				work.push(Work::CopyVolume {
					vol: Vol::Stack {
						stack: old.id,
						key: key.clone(),
					},
					dest: translate::volume_name(new.id.0, &key),
					label: key,
					owner: new.name.clone(),
				});
			}
		}
	}
	if options.deploy {
		work.extend(apps.into_iter().map(|(_, new)| Work::DeployApp(new)));
		work.extend(
			stack_list
				.into_iter()
				.map(|(_, new)| Work::DeployStack(new)),
		);
		work.extend(model_list.into_iter().map(Work::StartModel));
	}
	Ok(work)
}

/// Gives each cloned stack the service rows of its source: the images its deploy compares
/// against. The domains are not copied.
///
/// # Errors
/// The database fails.
pub async fn sync_stack_services(s: &AppState, clone: &EnvironmentClone) -> AppResult<()> {
	for p in clone.pairs.0.iter().filter(|p| p.kind == Kind::Stack) {
		let images = stacks::list_services(&s.db, StackId(p.source))
			.await?
			.into_iter()
			.map(|svc| (svc.name, svc.image))
			.collect();
		stacks::sync_services(&s.db, StackId(p.id), &images).await?;
	}
	Ok(())
}

/// How a step ended.
enum Outcome {
	Done(Option<String>),
	Skipped(String),
	Failed(String),
}

/// Polls `check` until it says something, the limit passes or the job is cancelled.
async fn wait<T, F, Fut>(cancel: &CancellationToken, mut check: F) -> Result<T, String>
where
	F: FnMut() -> Fut,
	Fut: Future<Output = Result<Option<T>, String>>,
{
	let deadline = tokio::time::Instant::now() + STEP_LIMIT;
	loop {
		if let Some(v) = check().await? {
			return Ok(v);
		}
		if tokio::time::Instant::now() >= deadline {
			return Err("it took longer than an hour".to_owned());
		}
		tokio::select! {
			() = cancel.cancelled() => return Err("cancelled".to_owned()),
			() = tokio::time::sleep(POLL) => {}
		}
	}
}

fn sql(e: &sqlx::Error) -> String {
	tracing::error!("environment clone database error: {e}");
	"database error".to_owned()
}

async fn provision(state: &AppState, cancel: &CancellationToken, db: &Database) -> Outcome {
	if let Err(e) = databases::enqueue(state, JobKind::DatabaseProvision, db).await {
		return Outcome::Failed(e.to_string());
	}
	let id = db.id;
	let waited = wait(cancel, || async {
		let row = databases::find_by_id(&state.db, id)
			.await
			.map_err(|e| sql(&e))?
			.ok_or_else(|| "the database is gone".to_owned())?;
		match row.status {
			DatabaseStatus::Running => Ok(Some(())),
			DatabaseStatus::Failed | DatabaseStatus::Deleting => Err(row
				.status_message
				.unwrap_or_else(|| "it did not start".to_owned())),
			_ => Ok(None),
		}
	})
	.await;
	match waited {
		Ok(()) => Outcome::Done(Some("running".to_owned())),
		Err(m) => Outcome::Failed(m),
	}
}

async fn copy_database(
	state: &AppState,
	cancel: &CancellationToken,
	user: UserId,
	target: BackupTargetId,
	source: &Database,
	clone: &Database,
) -> Outcome {
	let Ok(Some(source)) = databases::find_by_id(&state.db, source.id).await else {
		return Outcome::Failed("the source database is gone".to_owned());
	};
	if source.status != DatabaseStatus::Running {
		return Outcome::Failed(format!(
			"{} is not running, so there is nothing to dump; start it and copy its data again",
			source.name
		));
	}
	let backup = match backups::queue(state, source.id, source.server_id, target, Some(user)).await
	{
		Ok(b) => b,
		Err(e) => return Outcome::Failed(e.to_string()),
	};
	let dumped = wait(cancel, || async {
		let row = backups::find_by_id(&state.db, backup.id)
			.await
			.map_err(|e| sql(&e))?
			.ok_or_else(|| "the dump is gone".to_owned())?;
		match row.status {
			BackupStatus::Succeeded => Ok(Some(row)),
			BackupStatus::Failed => Err(format!(
				"the dump failed: {}",
				row.error.unwrap_or_else(|| "no reason given".to_owned())
			)),
			_ => Ok(None),
		}
	})
	.await;
	let dumped = match dumped {
		Ok(d) => d,
		Err(m) => return Outcome::Failed(m),
	};

	// The restore job restores a backup into the database the backup belongs to, so the
	// dump is lent to the clone as a backup row of its own, which only points at the same
	// object and is removed again without touching it.
	let loan = match backups::insert(&state.db, clone.id, target, Some(user)).await {
		Ok(b) => b,
		Err(e) => return Outcome::Failed(sql(&e)),
	};
	let outcome = restore_loan(state, cancel, clone, loan.id.0, &dumped).await;
	if let Err(e) = sqlx::query("DELETE FROM backups WHERE id = $1")
		.bind(loan.id)
		.execute(&state.db)
		.await
	{
		tracing::warn!(backup = loan.id.0, "cannot remove the lent backup row: {e}");
	}
	outcome
}

/// How often a restore into a database that is still starting is tried.
const RESTORE_TRIES: u32 = 8;
const RESTORE_RETRY_AFTER: Duration = Duration::from_secs(5);

async fn restore_loan(
	state: &AppState,
	cancel: &CancellationToken,
	clone: &Database,
	loan: i64,
	dumped: &backups::Backup,
) -> Outcome {
	if let Err(e) = sqlx::query(
		"UPDATE backups SET status = 'succeeded', object_key = $2, size_bytes = $3, sha256 = $4,
			finished_at = NOW() WHERE id = $1",
	)
	.bind(loan)
	.bind(&dumped.object_key)
	.bind(dumped.size_bytes)
	.bind(&dumped.sha256)
	.execute(&state.db)
	.await
	{
		return Outcome::Failed(sql(&e));
	}
	// A fresh database reports running while its first-start init still restarts the server
	// once, so a restore that finds no socket is tried again. The dump is `--clean`, so a
	// second try is safe.
	let mut waited = Err("the restore did not start".to_owned());
	for attempt in 0..RESTORE_TRIES {
		if let Err(e) = jobs::enqueue(
			state,
			JobKind::Restore,
			clone.server_id,
			loan,
			format!("db:{}", clone.id),
		)
		.await
		{
			return Outcome::Failed(e.to_string());
		}
		waited = wait(cancel, || async {
			let job = jobs::latest_for(&state.db, JobKind::Restore, loan)
				.await
				.map_err(|e| sql(&e))?
				.ok_or_else(|| "the restore job is gone".to_owned())?;
			match job.status {
				JobStatus::Succeeded => Ok(Some(())),
				JobStatus::Failed => {
					Err(job.error.unwrap_or_else(|| "the restore failed".to_owned()))
				}
				JobStatus::Cancelled => Err("the restore was cancelled".to_owned()),
				_ => Ok(None),
			}
		})
		.await;
		match &waited {
			Err(m) if attempt + 1 < RESTORE_TRIES && m.contains("connection to server") => {
				tokio::select! {
					() = cancel.cancelled() => return Outcome::Failed("cancelled".to_owned()),
					() = tokio::time::sleep(RESTORE_RETRY_AFTER) => {}
				}
			}
			_ => break,
		}
	}
	match waited {
		Ok(()) => Outcome::Done(Some("restored from a dump".to_owned())),
		Err(m) => Outcome::Failed(format!("the dump was taken but restoring it failed: {m}")),
	}
}

async fn copy_volume(
	state: &AppState,
	cancel: &CancellationToken,
	clone: &EnvironmentClone,
	user: UserId,
	target: BackupTargetId,
	vol: &Vol,
	dest: &str,
) -> Outcome {
	let subject = match subject::resolve(state, clone.team_id, clone.project_id, vol).await {
		Ok(s) => s,
		Err(e) => return Outcome::Failed(e.to_string()),
	};
	let policy = match volume_backups::ensure_policy(&state.db, &subject).await {
		Ok(p) => p,
		Err(e) => return Outcome::Failed(sql(&e)),
	};
	match volume_backups::busy(&state.db, policy.id).await {
		Ok(false) => {}
		Ok(true) => {
			return Outcome::Failed(
				"a backup or restore of the source volume is already running".to_owned(),
			);
		}
		Err(e) => return Outcome::Failed(sql(&e)),
	}
	let backup =
		match volume_backups::queue_backup(state, &subject, &policy, target, Some(user)).await {
			Ok(b) => b,
			Err(e) => return Outcome::Failed(sql(&e)),
		};
	let archived = wait(cancel, || async {
		let row = volume_backups::backup_by_id(&state.db, backup.id)
			.await
			.map_err(|e| sql(&e))?
			.ok_or_else(|| "the archive is gone".to_owned())?;
		match row.status {
			BackupStatus::Succeeded => Ok(Some(row)),
			BackupStatus::Failed => Err(format!(
				"the archive failed: {}",
				row.error.unwrap_or_else(|| "no reason given".to_owned())
			)),
			_ => Ok(None),
		}
	})
	.await;
	let archived = match archived {
		Ok(a) => a,
		Err(m) => return Outcome::Failed(m),
	};
	let restore =
		match volume_backups::insert_restore(&state.db, archived.id, RestoreMode::New, dest, user)
			.await
		{
			Ok(r) => r,
			Err(e) => return Outcome::Failed(sql(&e)),
		};
	if let Err(e) = jobs::enqueue(
		state,
		JobKind::VolumeRestore,
		subject.server,
		restore.id.0,
		subject.lock_key(),
	)
	.await
	{
		return Outcome::Failed(e.to_string());
	}
	let waited = wait(cancel, || async {
		let row = volume_backups::restore_by_id(&state.db, restore.id)
			.await
			.map_err(|e| sql(&e))?
			.ok_or_else(|| "the restore is gone".to_owned())?;
		match row.status {
			RestoreState::Succeeded => Ok(Some(())),
			RestoreState::Failed | RestoreState::Cancelled => Err(row
				.error
				.unwrap_or_else(|| "the restore did not finish".to_owned())),
			_ => Ok(None),
		}
	})
	.await;
	match waited {
		Ok(()) => Outcome::Done(Some("copied through the backup destination".to_owned())),
		Err(m) => Outcome::Failed(format!(
			"the archive was taken but restoring it failed: {m}"
		)),
	}
}

async fn execute(
	state: &AppState,
	cancel: &CancellationToken,
	clone: &EnvironmentClone,
	work: &Work,
	failed_databases: &HashSet<i64>,
) -> Outcome {
	let options = &clone.options.0;
	let (Some(user), target) = (clone.created_by, options.backup_target_id) else {
		return Outcome::Failed("the person who asked for the clone no longer exists".to_owned());
	};
	match work {
		Work::Provision(db) => provision(state, cancel, db).await,
		Work::CopyDatabase { source, clone: new } => {
			if failed_databases.contains(&new.id.0) {
				return Outcome::Skipped("the cloned database did not start".to_owned());
			}
			let Some(target) = target else {
				return Outcome::Failed("no backup destination was chosen".to_owned());
			};
			copy_database(state, cancel, user, target, source, new).await
		}
		Work::CopyVolume { vol, dest, .. } => {
			let Some(target) = target else {
				return Outcome::Failed("no backup destination was chosen".to_owned());
			};
			copy_volume(state, cancel, clone, user, target, vol, dest).await
		}
		Work::DeployApp(app) => match deployments::queue(state, app, Some(user), None).await {
			Ok(_) => Outcome::Done(Some("deploy queued".to_owned())),
			Err(e) => Outcome::Failed(e.to_string()),
		},
		Work::DeployStack(stack) => match stacks::service::start_deploy(state, stack).await {
			Ok(_) => Outcome::Done(Some("deploy queued".to_owned())),
			Err(e) => Outcome::Failed(e.to_string()),
		},
		Work::StartModel(model) => match models::enqueue(state, JobKind::ModelStart, model).await {
			Ok(()) => Outcome::Done(Some("start queued".to_owned())),
			Err(e) => Outcome::Failed(e.to_string()),
		},
	}
}

/// What the finished steps add up to.
#[must_use]
pub fn status_of(report: &[Step]) -> CloneStatus {
	let failed = report
		.iter()
		.filter(|s| s.status == StepStatus::Failed)
		.count();
	if failed == 0 {
		CloneStatus::Succeeded
	} else if failed == report.len() {
		CloneStatus::Failed
	} else {
		CloneStatus::Partial
	}
}

/// Runs one Environment clone job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires; [`JobFailure::Failed`] naming how many
/// steps failed (the clone's report says which, and the clone is `partial` or `failed`).
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let clone = find_by_id(&state.db, CloneId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the clone is gone"))?;
	let work = work_list(state, &clone).await.map_err(failed)?;
	let mut report: Vec<Step> = work.iter().map(Work::step).collect();
	save_report(&state.db, clone.id, CloneStatus::Running, &report)
		.await
		.map_err(|e| db_failure(&e))?;
	log.push(
		LogStream::System,
		&format!(
			"cloning {} into {}: {} steps",
			clone.source_name,
			clone.name,
			work.len()
		),
	)
	.await;

	let mut failed_databases: HashSet<i64> = HashSet::new();
	for (i, step) in work.iter().enumerate() {
		if cancel.is_cancelled() {
			for rest in &mut report[i..] {
				rest.status = StepStatus::Skipped;
				rest.detail = Some("cancelled".to_owned());
			}
			save_report(&state.db, clone.id, CloneStatus::Failed, &report)
				.await
				.map_err(|e| db_failure(&e))?;
			return Err(JobFailure::Cancelled);
		}
		report[i].status = StepStatus::Running;
		save_report(&state.db, clone.id, CloneStatus::Running, &report)
			.await
			.map_err(|e| db_failure(&e))?;
		log.push(LogStream::System, &report[i].label).await;

		let outcome = execute(state, cancel, &clone, step, &failed_databases).await;
		let (status, detail) = match outcome {
			Outcome::Done(d) => (StepStatus::Ok, d),
			Outcome::Skipped(m) => (StepStatus::Skipped, Some(m)),
			Outcome::Failed(m) => {
				if let Work::Provision(db) = step {
					failed_databases.insert(db.id.0);
				}
				(StepStatus::Failed, Some(m))
			}
		};
		report[i].status = status;
		report[i].detail = detail;
		let stream = if status == StepStatus::Failed {
			LogStream::Stderr
		} else {
			LogStream::System
		};
		log.push(
			stream,
			&format!(
				"  {}{}",
				match status {
					StepStatus::Ok => "ok",
					StepStatus::Skipped => "skipped",
					_ => "failed",
				},
				report[i]
					.detail
					.as_ref()
					.map(|d| format!(": {d}"))
					.unwrap_or_default()
			),
		)
		.await;
	}

	let status = status_of(&report);
	save_report(&state.db, clone.id, status, &report)
		.await
		.map_err(|e| db_failure(&e))?;
	let failed_count = report
		.iter()
		.filter(|s| s.status == StepStatus::Failed)
		.count();
	if failed_count > 0 {
		return Err(JobFailure::Failed(format!(
			"{failed_count} of {} steps failed; the environment is made, see the report",
			report.len()
		)));
	}
	Ok(())
}
