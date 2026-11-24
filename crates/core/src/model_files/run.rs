//! The `ModelDownload` job: asks the server's agent to run the downloader and
//! mirrors its progress onto the `model_files` row. Every `Err` is mapped onto
//! the row by the worker (`jobs::fail_target`).

use axum::http::Method;
use futures_util::StreamExt;
use proto::{Event, ModelDownloadRequest, Stream};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{ModelFile, ModelFileStatus, redact, token};
use crate::error::ServerError;
use crate::ids::ModelFileId;
use crate::jobs::{Job, JobFailure};
use crate::logs::JobLog;
use crate::servers;
use crate::state::AppState;

const LOST_CONNECTION: &str = "lost connection to server agent";

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("model download database error: {e}");
	failed("database error")
}

/// Runs one download. Running it again for the same model file resumes it.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`] with a message safe to show the model file's owner.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let file = super::find_by_id(&state.db, ModelFileId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("model file not found"))?;
	let server = servers::find(&state.db, file.server_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("target server not found"))?;
	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;
	let hf_token = token::fetch(&state.db, &state.secrets, file.team_id)
		.await
		.map_err(|_| failed("failed to read the Hugging Face token"))?;

	sqlx::query(
		"UPDATE model_files SET status = 'downloading', error = NULL, updated_at = NOW()
		WHERE id = $1",
	)
	.bind(file.id)
	.execute(&state.db)
	.await
	.map_err(|e| db_failure(&e))?;

	let request = ModelDownloadRequest {
		volume: file.volume_name.clone(),
		repo: file.repo.clone(),
		revision: file.revision.clone(),
		include: file.selection.patterns(),
		token: hf_token.clone(),
	};
	let mut events = client
		.stream(Method::POST, "/v1/model-downloads", Some(&request))
		.await
		.map_err(|e| {
			warn!(server = %server.id, "failed to start the download stream: {e}");
			failed("server agent unavailable")
		})?;

	let secret = hf_token.as_deref();
	let mut progress = Progress::new(&file);
	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &redact(&line, secret)).await,
			Some(Ok(Event::Progress { bytes })) => {
				progress.update(state, log, &file, bytes).await?;
			}
			Some(Ok(Event::Done)) => return finish(state, log, &file).await,
			Some(Ok(Event::Failed { message })) => {
				return Err(JobFailure::Failed(redact(&message, secret)));
			}
			Some(Ok(_)) => {}
			Some(Err(e)) => {
				warn!(model_file = file.id.0, "download stream failed: {e}");
				return Err(failed(LOST_CONNECTION));
			}
			None => return Err(failed(LOST_CONNECTION)),
		}
	}
}

/// Tracks what was last reported, so the row is written on every progress tick
/// but the log only gets a line when the whole-percent figure moves.
struct Progress {
	total: u64,
	percent_logged: u64,
}

impl Progress {
	fn new(file: &ModelFile) -> Self {
		Self {
			total: u64::try_from(file.total_bytes).unwrap_or(0),
			percent_logged: 0,
		}
	}

	async fn update(
		&mut self,
		state: &AppState,
		log: &mut JobLog,
		file: &ModelFile,
		volume_bytes: u64,
	) -> Result<(), JobFailure> {
		// The volume also holds hf's bookkeeping, so it can exceed the files' size.
		let bytes = volume_bytes.min(self.total);
		sqlx::query(
			"UPDATE model_files SET downloaded_bytes = $2, updated_at = NOW() WHERE id = $1",
		)
		.bind(file.id)
		.bind(i64::try_from(bytes).unwrap_or(i64::MAX))
		.execute(&state.db)
		.await
		.map_err(|e| db_failure(&e))?;
		let percent = percent(bytes, self.total);
		if percent > self.percent_logged {
			self.percent_logged = percent;
			log.push(
				Stream::System,
				&format!("downloaded {bytes} of {} bytes ({percent}%)", self.total),
			)
			.await;
		}
		Ok(())
	}
}

fn percent(done: u64, total: u64) -> u64 {
	if total == 0 {
		return 100;
	}
	u64::try_from(u128::from(done) * 100 / u128::from(total)).unwrap_or(100)
}

async fn finish(state: &AppState, log: &mut JobLog, file: &ModelFile) -> Result<(), JobFailure> {
	sqlx::query(
		"UPDATE model_files SET status = $2, downloaded_bytes = total_bytes, error = NULL,
			updated_at = NOW() WHERE id = $1",
	)
	.bind(file.id)
	.bind(ModelFileStatus::Ready)
	.execute(&state.db)
	.await
	.map_err(|e| db_failure(&e))?;
	log.push(Stream::System, "download complete").await;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn percent_is_whole_and_safe() {
		assert_eq!(percent(0, 100), 0);
		assert_eq!(percent(50, 100), 50);
		assert_eq!(percent(100, 100), 100);
		assert_eq!(percent(1, 3), 33);
		assert_eq!(percent(5, 0), 100);
		assert_eq!(percent(u64::MAX, u64::MAX), 100);
	}
}
