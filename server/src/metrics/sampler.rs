//! The sampler: every interval, one `/v1/stats` per active server.
//!
//! Own loop, own timeout, own failure handling. A server that does not answer
//! simply gets no row for that tick; whether it is `unreachable` is decided
//! by the status sync alone.

use std::time::Duration;

use futures_util::StreamExt;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::servers::{self, Server, ServerStatus};
use crate::state::AppState;

const CONCURRENT_SERVERS: usize = 8;

/// Samples every active server each `interval` until `shutdown`.
pub async fn run(state: AppState, interval: Duration, shutdown: CancellationToken) {
	loop {
		sweep(&state).await;
		tokio::select! {
			() = shutdown.cancelled() => return,
			() = tokio::time::sleep(interval) => {}
		}
	}
}

async fn sweep(state: &AppState) {
	let servers = match servers::list_for_sync(&state.db).await {
		Ok(servers) => servers,
		Err(e) => {
			tracing::warn!(error = %e, "metrics sampler could not list servers");
			return;
		}
	};
	futures_util::stream::iter(
		servers
			.into_iter()
			.filter(|s| s.status == ServerStatus::Active),
	)
	.for_each_concurrent(CONCURRENT_SERVERS, |server| async move {
		sample(state, &server).await;
	})
	.await;
}

async fn sample(state: &AppState, server: &Server) {
	let agent = match state.agents.client(state, server).await {
		Ok(agent) => agent,
		Err(e) => {
			tracing::debug!(server = %server.id, error = %e, "metrics: no agent client");
			return;
		}
	};
	let sample = match agent.stats().await {
		Ok(sample) => sample,
		Err(e) => {
			tracing::debug!(server = %server.id, error = %e, "metrics: could not fetch stats");
			return;
		}
	};
	if let Err(e) = super::record(&state.db, server.id, OffsetDateTime::now_utc(), &sample).await {
		tracing::warn!(server = %server.id, error = %e, "metrics: could not store a sample");
	}
}
