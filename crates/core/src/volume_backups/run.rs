//! The Volume backup and Volume restore jobs.
//!
//! A backup archives the volume on the server's agent, then uploads the archive as
//! the parts of an S3 multipart upload. With [`Consistency::Stop`] the owner is
//! stopped only while the agent reads the volume and started again before the upload,
//! whatever the read did. A restore stops the owner, has the agent write the archive
//! into the volume, and starts the owner again, moving through [`RestoreState`].

use proto::{
	Event, Stream as LogStream, UploadPartsRequest, VolumeDumpRequest, VolumeRestoreRequest,
};
use reqwest::Method;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::subject::{self, Owner, Subject};
use super::{
	Consistency, Policy, Restore, RestoreMode, RestoreState, Stored, VolumeBackup, advance_restore,
	backup_by_id, beyond_retention, delete_backup_row, mark_running, mark_succeeded, object_key,
	plan_parts, policy_by_id, restore_by_id,
};
use crate::agents::AgentClient;
use crate::backups::{
	self, BackupTarget, call, db_failure, decrypt_secret, failed, find_target, s3, team_of,
};
use crate::error::ServerError;
use crate::ids::{BackupTargetId, TeamId, VolumeBackupId, VolumeRestoreId};
use crate::jobs::{Job, JobFailure};
use crate::logs::JobLog;
use crate::stacks::{self, ServiceStatus};
use crate::state::AppState;
use crate::{deployments, scope};
use zeroize::Zeroizing;

/// Removes the backup's object from its destination, then the row. A backup that
/// failed has no object and is only deleted.
///
/// # Errors
/// [`ServerError::Validation`] with the storage error when the object cannot be
/// deleted; the row is kept so the delete can be retried.
pub async fn purge(state: &AppState, backup: &VolumeBackup) -> Result<(), ServerError> {
	if let (Some(key), Some(target_id)) = (&backup.object_key, backup.target_id)
		&& let Some(target) = find_target(&state.db, target_id).await?
	{
		let secret = decrypt_secret(&state.secrets, &target).map_err(|e| {
			tracing::error!(
				backup_target = target.id.0,
				"cannot decrypt backup target secret: {e}"
			);
			ServerError::Internal
		})?;
		s3::delete(&state.outbound, &target, &secret, key)
			.await
			.map_err(|e| {
				ServerError::Validation(format!("could not delete the backup from storage: {e}"))
			})?;
	}
	delete_backup_row(&state.db, backup.id).await?;
	Ok(())
}

/// Starts and stops the containers that use a volume.
struct Containers<'a> {
	state: &'a AppState,
	client: &'a AgentClient,
	subject: &'a Subject,
}

impl Containers<'_> {
	/// The containers running now: an application's current deployment, or every
	/// running service of a stack.
	async fn running(&self) -> Result<Vec<String>, JobFailure> {
		match self.subject.owner {
			Owner::App(app) => Ok(deployments::latest_running(&self.state.db, app)
				.await
				.map_err(|e| db_failure(&e))?
				.and_then(|d| d.container_id)
				.into_iter()
				.collect()),
			Owner::Stack(stack) => Ok(stacks::list_services(&self.state.db, stack)
				.await
				.map_err(|e| db_failure(&e))?
				.into_iter()
				.filter(|s| s.status == ServiceStatus::Running)
				.filter_map(|s| s.container_id)
				.collect()),
		}
	}

	/// Stops each container in turn. On failure, returns those already stopped
	/// along with the failure, so the caller can start them again.
	async fn stop(&self, containers: &[String]) -> Result<Vec<String>, (Vec<String>, JobFailure)> {
		let mut stopped = Vec::new();
		for container in containers {
			if let Err(e) = self
				.client
				.send(Method::POST, &format!("/v1/containers/{container}/stop"))
				.await
			{
				warn!(
					container,
					"cannot stop a container for a volume backup: {e}"
				);
				return Err((
					stopped,
					failed(format!("could not stop {}: {e}", self.subject.owner_name)),
				));
			}
			stopped.push(container.clone());
		}
		Ok(stopped)
	}

	/// Starts the containers again, last stopped first. Every one is tried; the
	/// message names what did not start.
	async fn start(&self, containers: &[String]) -> Option<String> {
		let mut problems = Vec::new();
		for container in containers.iter().rev() {
			if let Err(e) = self
				.client
				.send(Method::POST, &format!("/v1/containers/{container}/start"))
				.await
			{
				warn!(
					container,
					"cannot start a container after a volume job: {e}"
				);
				problems.push(e.to_string());
			}
		}
		(!problems.is_empty()).then(|| {
			format!(
				"could not start {} again: {}",
				self.subject.owner_name,
				problems.join("; ")
			)
		})
	}
}

/// Everything a job needs, resolved up front so a missing piece fails the job before
/// it touches the agent.
struct Prepared {
	policy: Policy,
	subject: Subject,
	team: TeamId,
	target: BackupTarget,
	secret: Zeroizing<String>,
	client: AgentClient,
}

async fn prepare(
	state: &AppState,
	policy: Policy,
	target: Option<BackupTargetId>,
) -> Result<Prepared, JobFailure> {
	let subject = subject::of_policy(state, &policy)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the volume no longer exists"))?;
	let target_id = target.ok_or_else(|| failed("the backup destination was deleted"))?;
	let target = find_target(&state.db, target_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("backup destination not found"))?;
	let secret = decrypt_secret(&state.secrets, &target).map_err(|e| {
		warn!(
			backup_target = target.id.0,
			"cannot decrypt backup target secret: {e}"
		);
		failed("cannot decrypt the backup destination's secret")
	})?;
	let team = team_of(&state.db, subject.project).await?;
	if target.team_id != team {
		return Err(failed("backup destination belongs to another team"));
	}
	let server = scope::server(&state.db, team, subject.server)
		.await
		.map_err(|e| match e {
			ServerError::ServerNotFound => failed("target server not found"),
			ServerError::Sqlx(e) => db_failure(&e),
			_ => failed("failed to look up target server"),
		})?;
	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;
	Ok(Prepared {
		policy,
		subject,
		team,
		target,
		secret,
		client,
	})
}

/// The agent's answer to an upload request that carries no part urls: it refuses it
/// and deletes the archive file, which is how a job that gives up after the dump
/// frees the agent's disk.
async fn discard(client: &AgentClient, path: &str) {
	let request = UploadPartsRequest {
		path: path.to_owned(),
		part_size: 1,
		urls: Vec::new(),
	};
	let _ = client
		.stream(Method::POST, "/v1/volume-backups/upload", Some(&request))
		.await;
}

/// Runs one Volume backup job: archive on the agent, upload as a multipart upload,
/// record the result, then prune the backups beyond the policy's retention.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`]; the
/// worker mirrors it onto the backup row.
pub async fn run_backup(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let backup = backup_by_id(&state.db, VolumeBackupId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("backup not found"))?;
	let policy = policy_by_id(&state.db, backup.policy_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the volume's backup settings are gone"))?;
	let p = prepare(state, policy, backup.target_id).await?;
	mark_running(&state.db, backup.id)
		.await
		.map_err(|e| db_failure(&e))?;

	let containers = Containers {
		state,
		client: &p.client,
		subject: &p.subject,
	};
	let running = containers.running().await?;
	let mut warning = None;
	let mut stopped = Vec::new();
	match backup.consistency {
		Consistency::Stop if !running.is_empty() => {
			log.push(
				LogStream::System,
				&format!("stopping {}", p.subject.owner_name),
			)
			.await;
			match containers.stop(&running).await {
				Ok(done) => stopped = done,
				Err((done, failure)) => {
					containers.start(&done).await;
					return Err(failure);
				}
			}
		}
		Consistency::Live if !running.is_empty() => {
			warning = Some(format!(
				"{} kept running while the volume was read, so files written during the backup may not agree with each other. Choose \"stop during backup\" for a consistent copy.",
				p.subject.owner_name
			));
		}
		_ => {}
	}

	let dump = VolumeDumpRequest {
		backup_id: backup.id.0,
		volume: p.subject.volume_name.clone(),
	};
	let dumped = call(&p.client, "/v1/volume-backups/dump", &dump, cancel, log).await;
	// Started again whatever the read did, before the long upload.
	if !stopped.is_empty() {
		log.push(
			LogStream::System,
			&format!("starting {}", p.subject.owner_name),
		)
		.await;
		if let Some(problem) = containers.start(&stopped).await {
			log.push(LogStream::Stderr, &problem).await;
			warning = Some(format!(
				"The backup was taken, but {problem}. Start it from its page."
			));
		}
	}
	let Event::VolumeDumped {
		path,
		size,
		sha256,
		entries,
		raw_bytes,
	} = dumped?
	else {
		return Err(failed("server agent finished the archive without a file"));
	};

	let key = object_key(&p.target.prefix, p.team, p.policy.id, backup.id);
	let uploaded = upload(state, &p, &key, &path, size, cancel, log).await;
	if let Err(e) = uploaded {
		discard(&p.client, &path).await;
		return Err(e);
	}

	let to_i64 = |n: u64| i64::try_from(n).map_err(|_| failed("archive size out of range"));
	mark_succeeded(
		&state.db,
		backup.id,
		&Stored {
			key: &key,
			size: to_i64(size)?,
			raw_bytes: to_i64(raw_bytes)?,
			entries: to_i64(entries)?,
			sha256: &sha256,
			warning: warning.as_deref(),
		},
	)
	.await
	.map_err(|e| db_failure(&e))?;
	log.push(LogStream::System, &format!("backup stored as {key}"))
		.await;

	enforce_retention(state, &p.policy, log).await;
	Ok(())
}

/// Sends the archive file at `path` to `key` as a multipart upload. The agent sends
/// the parts; this makes the other three requests. An upload that does not complete
/// is aborted so its parts do not stay in the bucket.
async fn upload(
	state: &AppState,
	p: &Prepared,
	key: &str,
	path: &str,
	size: u64,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let (part_size, count) = plan_parts(size);
	let upload_id = s3::create_multipart(&state.outbound, &p.target, &p.secret, key)
		.await
		.map_err(|e| failed(e.to_string()))?;
	let sent = async {
		let urls = s3::part_urls(&p.target, &p.secret, key, &upload_id, count)
			.map_err(|e| failed(e.to_string()))?;
		let request = UploadPartsRequest {
			path: path.to_owned(),
			part_size,
			urls: urls.iter().map(ToString::to_string).collect(),
		};
		let Event::PartsUploaded { etags } = call(
			&p.client,
			"/v1/volume-backups/upload",
			&request,
			cancel,
			log,
		)
		.await?
		else {
			return Err(failed(
				"server agent finished the upload without confirming it",
			));
		};
		s3::complete_multipart(
			&state.outbound,
			&p.target,
			&p.secret,
			key,
			&upload_id,
			&etags,
		)
		.await
		.map_err(|e| failed(e.to_string()))
	}
	.await;
	if sent.is_err()
		&& let Err(e) =
			s3::abort_multipart(&state.outbound, &p.target, &p.secret, key, &upload_id).await
	{
		warn!("cannot abort a multipart upload: {e}");
	}
	sent
}

/// Best effort: the new backup already succeeded, so a failure here is logged and
/// retried by the next backup.
async fn enforce_retention(state: &AppState, policy: &Policy, log: &mut JobLog) {
	let old = match beyond_retention(&state.db, policy.id, i64::from(policy.keep)).await {
		Ok(old) => old,
		Err(e) => {
			warn!(
				policy = policy.id.0,
				"cannot list volume backups beyond retention: {e}"
			);
			return;
		}
	};
	for backup in old {
		match purge(state, &backup).await {
			Ok(()) => {
				log.push(
					LogStream::System,
					&format!("removed old backup {}", backup.id),
				)
				.await;
			}
			Err(e) => warn!(
				backup = backup.id.0,
				"cannot remove volume backup beyond retention: {e}"
			),
		}
	}
}

fn state_failure(e: &super::AdvanceError) -> JobFailure {
	warn!("volume restore state change refused: {e}");
	failed("the restore was changed while it ran")
}

/// Moves the restore along its state machine.
async fn step(
	state: &AppState,
	id: VolumeRestoreId,
	from: RestoreState,
	to: RestoreState,
) -> Result<(), JobFailure> {
	advance_restore(&state.db, id, from, to, None)
		.await
		.map_err(|e| state_failure(&e))
}

/// What a restore job needs, resolved up front.
struct RestorePlan {
	restore: Restore,
	p: Prepared,
	request: VolumeRestoreRequest,
}

async fn plan_restore(state: &AppState, job: &Job) -> Result<RestorePlan, JobFailure> {
	let restore = restore_by_id(&state.db, VolumeRestoreId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("restore not found"))?;
	let backup = backup_by_id(&state.db, restore.backup_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("backup not found"))?;
	let policy = policy_by_id(&state.db, backup.policy_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("the volume's backup settings are gone"))?;
	let p = prepare(state, policy, backup.target_id).await?;
	let key = backup
		.object_key
		.as_deref()
		.filter(|_| backup.status == backups::BackupStatus::Succeeded)
		.ok_or_else(|| failed("backup has no stored archive"))?;
	let sha256 = backup
		.sha256
		.clone()
		.ok_or_else(|| failed("backup has no recorded checksum"))?;
	let url = s3::presign(&p.target, &p.secret, s3::Method::Get, key)
		.map_err(|e| failed(e.to_string()))?;
	let request = VolumeRestoreRequest {
		volume: restore.dest_volume.clone(),
		url: url.to_string(),
		sha256,
		wipe: restore.mode == RestoreMode::Same,
	};
	Ok(RestorePlan {
		restore,
		p,
		request,
	})
}

/// Runs one Volume restore job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`]; the
/// worker mirrors it onto the restore row. What was stopped is started again first.
pub async fn run_restore(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let RestorePlan {
		restore,
		p,
		request,
	} = plan_restore(state, job).await?;
	let containers = Containers {
		state,
		client: &p.client,
		subject: &p.subject,
	};
	let owner = &p.subject.owner_name;

	step(
		state,
		restore.id,
		RestoreState::Queued,
		RestoreState::Stopping,
	)
	.await?;
	let mut stopped = Vec::new();
	if restore.mode == RestoreMode::Same {
		let running = containers.running().await?;
		if !running.is_empty() {
			log.push(LogStream::System, &format!("stopping {owner}"))
				.await;
		}
		match containers.stop(&running).await {
			Ok(done) => stopped = done,
			Err((done, failure)) => {
				containers.start(&done).await;
				return Err(failure);
			}
		}
	}

	step(
		state,
		restore.id,
		RestoreState::Stopping,
		RestoreState::Restoring,
	)
	.await?;
	let restored = call(
		&p.client,
		"/v1/volume-backups/restore",
		&request,
		cancel,
		log,
	)
	.await;

	// Started again whatever the write did: a failed restore must not leave the
	// application down on top of everything else.
	let written = matches!(&restored, Ok(Event::Done));
	if written {
		step(
			state,
			restore.id,
			RestoreState::Restoring,
			RestoreState::Starting,
		)
		.await?;
	}
	let problem = if stopped.is_empty() {
		None
	} else {
		log.push(LogStream::System, &format!("starting {owner}"))
			.await;
		containers.start(&stopped).await
	};
	restored?;
	if !written {
		return Err(failed(
			"server agent finished the restore without confirming it",
		));
	}
	if let Some(problem) = problem {
		return Err(failed(format!("the archive was restored, but {problem}")));
	}
	step(
		state,
		restore.id,
		RestoreState::Starting,
		RestoreState::Succeeded,
	)
	.await?;
	log.push(
		LogStream::System,
		&format!("restored into {}", restore.dest_volume),
	)
	.await;
	Ok(())
}
