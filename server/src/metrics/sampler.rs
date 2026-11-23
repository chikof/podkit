//! The sampler: every interval, one `/v1/stats` per active server.
//!
//! Own loop, own timeout, own failure handling. A server that does not answer
//! simply gets no row for that tick; whether it is `unreachable` is decided
//! by the status sync alone.

use std::time::Duration;

use futures_util::StreamExt;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::collect::Shared;
use super::ext::ExtSamples;
use crate::agents::AgentClient;
use crate::databases::EngineKind;
use crate::error::ServerError;
use crate::servers::{self, Server, ServerStatus};
use crate::state::AppState;

const CONCURRENT_SERVERS: usize = 8;

/// Samples every active server each `interval` until `shutdown`.
pub async fn run(state: AppState, interval: Duration, shutdown: CancellationToken) {
	let memory = Shared::default();
	loop {
		sweep(&state, &memory).await;
		if let Err(e) =
			crate::notifications::alerts::evaluate(&state, OffsetDateTime::now_utc()).await
		{
			tracing::warn!(error = %e, "metric alerts could not be evaluated");
		}
		tokio::select! {
			() = shutdown.cancelled() => return,
			() = tokio::time::sleep(interval) => {}
		}
	}
}

async fn sweep(state: &AppState, memory: &Shared) {
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
		sample(state, memory, &server).await;
	})
	.await;
}

async fn sample(state: &AppState, memory: &Shared, server: &Server) {
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
	let now = OffsetDateTime::now_utc();
	if let Err(e) = super::record(&state.db, server.id, now, &sample).await {
		tracing::warn!(server = %server.id, error = %e, "metrics: could not store a sample");
	}
	sample_extras(state, memory, server, &agent, now, &sample).await;
}

/// The figures beyond CPU and memory: HTTP rates, volumes and VRAM from the stats already
/// fetched, database figures and container events from one more question each. Each source
/// fails on its own: a database that does not answer leaves the others untouched.
async fn sample_extras(
	state: &AppState,
	memory: &Shared,
	server: &Server,
	agent: &AgentClient,
	now: OffsetDateTime,
	sample: &proto::Stats,
) {
	let at = now.unix_timestamp();
	let mut out = ExtSamples::new();
	super::collect::volumes_and_vram(sample, &mut out);
	let databases = match database_stats(state, server, agent).await {
		Ok(rows) => rows,
		Err(e) => {
			tracing::debug!(server = %server.id, error = %e, "metrics: no database stats");
			Vec::new()
		}
	};
	let totals = match model_totals(&state.db, server).await {
		Ok(rows) => rows,
		Err(e) => {
			tracing::warn!(server = %server.id, error = %e, "metrics: could not read model usage");
			Vec::new()
		}
	};
	let window = {
		let Ok(mut all) = memory.lock() else { return };
		let mem = all.entry(server.id).or_default();
		mem.http(at, sample, &mut out);
		mem.databases(at, &databases, &mut out);
		mem.models(at, &totals, &mut out);
		mem.event_window(at)
	};
	if let Err(e) = super::ext::record(&state.db, now, &out).await {
		tracing::warn!(server = %server.id, error = %e, "metrics: could not store the extra series");
	}
	match agent.events(window.0, window.1).await {
		Ok(events) => match super::events::record(&state.db, &events).await {
			Ok(()) => {
				if let Ok(mut all) = memory.lock() {
					all.entry(server.id).or_default().events_stored(window.1);
				}
			}
			Err(e) => {
				tracing::warn!(server = %server.id, error = %e, "metrics: could not store events");
			}
		},
		Err(e) => {
			tracing::debug!(server = %server.id, error = %e, "metrics: could not fetch events");
		}
	}
}

/// Asks the agent for the databases that run on `server`. No question when there are none.
async fn database_stats(
	state: &AppState,
	server: &Server,
	agent: &AgentClient,
) -> Result<Vec<proto::DbStat>, ServerError> {
	let rows = sqlx::query_as::<_, (i64, EngineKind, String)>(
		"SELECT id, engine, container_id FROM databases
		WHERE server_id = $1 AND status = 'running' AND container_id IS NOT NULL",
	)
	.bind(server.id)
	.fetch_all(&state.db)
	.await?;
	if rows.is_empty() {
		return Ok(Vec::new());
	}
	agent
		.db_stats(
			rows.into_iter()
				.map(|(id, engine, container)| proto::DbStatsTarget {
					id,
					container,
					engine: engine.into(),
				})
				.collect(),
		)
		.await
}

/// `(model id, requests, tokens)` served so far by each model of `server`, from the usage the
/// status sync brought home.
async fn model_totals(
	db: &sqlx::PgPool,
	server: &Server,
) -> Result<Vec<(i64, i64, i64)>, sqlx::Error> {
	sqlx::query_as::<_, (i64, i64, i64)>(
		"SELECT mu.model_id, COALESCE(SUM(mu.requests), 0)::BIGINT,
			COALESCE(SUM(mu.input_tokens + mu.output_tokens), 0)::BIGINT
		FROM model_usage mu JOIN models m ON m.id = mu.model_id
		WHERE m.server_id = $1 GROUP BY mu.model_id",
	)
	.bind(server.id)
	.fetch_all(db)
	.await
}
