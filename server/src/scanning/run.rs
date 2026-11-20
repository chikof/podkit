//! The scan jobs. `ImageScan` resolves one queued scan: it reuses a recent scan
//! of the same image or has the agent run Syft and Grype. `ImageRescan` runs
//! Grype again over the SBOMs a server stored, which never reads an image.

use proto::ScanRequest;
use tokio_util::sync::CancellationToken;

use super::{compress, decompress, store};
use crate::error::ServerError;
use crate::ids::{ScanId, ServerId};
use crate::jobs::{Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::scope;
use crate::state::AppState;

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("scan job database error: {e}");
	failed("database error")
}

/// What the person who deployed the image reads when a scan fails.
fn agent_failure(e: &ServerError) -> JobFailure {
	failed(match e {
		ServerError::Validation(message) | ServerError::Conflict(message) => message.clone(),
		other => other.to_string(),
	})
}

/// Runs one scan job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise [`JobFailure::Failed`]
/// with a message safe to show next to the resource.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	match job.kind {
		JobKind::ImageScan => scan(state, ScanId(job.target_id), cancel, log).await,
		JobKind::ImageRescan => rescan(state, ServerId(job.target_id), cancel, log).await,
		_ => Ok(()),
	}
}

async fn scan(
	state: &AppState,
	id: ScanId,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let Some(scan) = store::find(&state.db, id)
		.await
		.map_err(|e| db_failure(&e))?
	else {
		return Ok(());
	};
	let settings = store::server_settings(&state.db, scan.server_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("server not found"))?;
	if !settings.scan_enabled {
		return Err(failed("scanning is off for this server"));
	}
	store::mark_scanning(&state.db, id)
		.await
		.map_err(|e| db_failure(&e))?;
	let server = scope::server(&state.db, scan.team_id, scan.server_id)
		.await
		.map_err(|e| agent_failure(&e))?;
	let client = state
		.agents
		.client(state, &server)
		.await
		.map_err(|e| agent_failure(&e))?;

	let digest = tokio::select! {
		() = cancel.cancelled() => return Err(JobFailure::Cancelled),
		resolved = client.scan_resolve(&scan.image_ref) => resolved.map_err(|e| agent_failure(&e))?,
	};
	store::set_digest(&state.db, id, &digest)
		.await
		.map_err(|e| db_failure(&e))?;

	if !scan.force
		&& let Some(earlier) = store::find_cached(
			&state.db,
			scan.server_id,
			&digest,
			id,
			time::OffsetDateTime::now_utc(),
		)
		.await
		.map_err(|e| db_failure(&e))?
	{
		log.push(
			proto::Stream::System,
			&format!("{digest} was scanned less than a day ago; showing that result"),
		)
		.await;
		store::merge_into(&state.db, id, earlier)
			.await
			.map_err(|e| db_failure(&e))?;
		return Ok(());
	}

	log.push(
		proto::Stream::System,
		&format!("scanning {} ({digest})", scan.image_ref),
	)
	.await;
	let request = ScanRequest {
		image: Some(scan.image_ref.clone()),
		sbom: None,
		cpus: settings.scan_cpus,
		memory_mb: u32::try_from(settings.scan_memory_mb).unwrap_or(1024),
	};
	let result = tokio::select! {
		() = cancel.cancelled() => return Err(JobFailure::Cancelled),
		result = client.scan(&request) => result.map_err(|e| agent_failure(&e))?,
	};
	let sbom = result.sbom.as_deref().map(|s| (compress(s), s.len()));
	let (packed, size) = match sbom {
		Some((Ok(packed), size)) => (Some(packed), i32::try_from(size).ok()),
		Some((Err(e), _)) => {
			tracing::warn!("cannot compress an SBOM: {e}");
			(None, None)
		}
		None => (None, None),
	};
	store::store_result(&state.db, id, &result, packed, size, false)
		.await
		.map_err(|e| db_failure(&e))?;
	log.push(
		proto::Stream::System,
		&format!("{} findings", result.findings.len()),
	)
	.await;
	Ok(())
}

async fn rescan(
	state: &AppState,
	server_id: ServerId,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let settings = store::server_settings(&state.db, server_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("server not found"))?;
	if !settings.scan_enabled {
		return Ok(());
	}
	let scans = store::running_scans(&state.db, server_id)
		.await
		.map_err(|e| db_failure(&e))?;
	if scans.is_empty() {
		return Ok(());
	}
	let server = scope::server(&state.db, settings.team_id, server_id)
		.await
		.map_err(|e| agent_failure(&e))?;
	let client = state
		.agents
		.client(state, &server)
		.await
		.map_err(|e| agent_failure(&e))?;

	let (mut done, mut errors) = (0_usize, 0_usize);
	for id in scans {
		if cancel.is_cancelled() {
			return Err(JobFailure::Cancelled);
		}
		let Some(packed) = store::sbom_of(&state.db, id)
			.await
			.map_err(|e| db_failure(&e))?
		else {
			continue;
		};
		let sbom = match decompress(&packed) {
			Ok(sbom) => sbom,
			Err(e) => {
				log.push(
					proto::Stream::System,
					&format!("scan {id}: unreadable SBOM: {e}"),
				)
				.await;
				errors += 1;
				continue;
			}
		};
		let request = ScanRequest {
			image: None,
			sbom: Some(sbom),
			cpus: settings.scan_cpus,
			memory_mb: u32::try_from(settings.scan_memory_mb).unwrap_or(1024),
		};
		let outcome = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			outcome = client.scan(&request) => outcome,
		};
		match outcome {
			Ok(result) => {
				store::store_result(&state.db, id, &result, None, None, true)
					.await
					.map_err(|e| db_failure(&e))?;
				done += 1;
			}
			Err(e) => {
				log.push(proto::Stream::System, &format!("scan {id}: {e}"))
					.await;
				errors += 1;
			}
		}
	}
	log.push(
		proto::Stream::System,
		&format!("rescanned {done} images, {errors} failed"),
	)
	.await;
	if done == 0 && errors > 0 {
		return Err(failed("no stored SBOM could be rescanned"));
	}
	Ok(())
}
