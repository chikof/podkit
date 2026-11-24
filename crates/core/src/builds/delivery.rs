//! Building on one server and running on another: the build is asked of the build server's
//! agent, and the image it makes is moved to the server that runs it.
//!
//! The move is the one `docs/design/cluster.md` recommends for v1: the control plane relays
//! `podman save` from the build server into `podman load` on the target, as one stream that
//! is never held in memory, and checks that the image on the target has the content id of
//! the one that was sent. A registry would only move changed layers; it is a service that has
//! to be running, and this is not (cluster.md, "Image distribution").

use futures_util::StreamExt;
use proto::{BuildRequest, Event, Stream};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agents::AgentClient;
use crate::jobs::JobFailure;
use crate::logs::JobLog;

/// The word shown when the agent cannot be reached; the cause is in the server's log.
const LOST: &str = "lost connection to the build server's agent";

/// Asks `builder` to clone and build, copying its output into the deployment log. Returns the
/// image reference and the commit that was built.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires (dropping the stream stops the build on the
/// agent); [`JobFailure::Failed`] with the build's own message, or [`LOST`].
pub async fn build(
	builder: &AgentClient,
	request: &BuildRequest,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(String, String), JobFailure> {
	let mut events = builder
		.stream(axum::http::Method::POST, "/v1/build", Some(request))
		.await
		.map_err(|e| {
			warn!("cannot start the build on the build server: {e}");
			JobFailure::Failed("the build server's agent is unavailable".to_owned())
		})?;
	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &line).await,
			Some(Ok(Event::Built { image, commit_sha })) => return Ok((image, commit_sha)),
			Some(Ok(Event::Failed { message })) => return Err(JobFailure::Failed(message)),
			Some(Ok(_)) => {}
			Some(Err(e)) => {
				warn!("build stream failed: {e}");
				return Err(JobFailure::Failed(LOST.to_owned()));
			}
			None => return Err(JobFailure::Failed(LOST.to_owned())),
		}
	}
}

/// Why an image did not arrive.
#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryError {
	/// The build server could not send it.
	Send(String),
	/// The target could not load it.
	Load(String),
	/// It loaded, but it is not the image that was sent.
	Mismatch { sent: String, got: String },
}

impl std::fmt::Display for DeliveryError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Send(why) => write!(f, "the build server could not send the image: {why}"),
			Self::Load(why) => write!(f, "the server could not load the image: {why}"),
			Self::Mismatch { sent, got } => {
				write!(
					f,
					"the image that arrived ({got}) is not the one sent ({sent})"
				)
			}
		}
	}
}

/// Moves `reference` from `from` to `to` and returns its content id. Both ends are agents; the
/// bytes flow from one to the other as they are produced.
///
/// # Errors
/// [`DeliveryError`] says which end failed, or that the content id differs.
pub async fn relay(
	from: &AgentClient,
	to: &AgentClient,
	reference: &str,
) -> Result<String, DeliveryError> {
	let (sent, archive) = from
		.export_image(reference)
		.await
		.map_err(|e| DeliveryError::Send(e.to_string()))?;
	let loaded = to
		.import_image(reference, archive)
		.await
		.map_err(|e| DeliveryError::Load(e.to_string()))?;
	if loaded.image_id == sent {
		Ok(sent)
	} else {
		Err(DeliveryError::Mismatch {
			sent,
			got: loaded.image_id,
		})
	}
}

/// [`relay`] for a deploy: logs the step and maps a failure onto the job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires; [`JobFailure::Failed`] otherwise.
pub async fn deliver(
	from: &AgentClient,
	to: &AgentClient,
	reference: &str,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	log.push(
		Stream::System,
		"sending the image to the server that runs it",
	)
	.await;
	let id = tokio::select! {
		() = cancel.cancelled() => return Err(JobFailure::Cancelled),
		moved = relay(from, to, reference) => moved,
	}
	.map_err(|e| {
		warn!("image delivery failed: {e}");
		JobFailure::Failed(e.to_string())
	})?;
	log.push(Stream::System, &format!("image delivered ({id})"))
		.await;
	Ok(())
}
