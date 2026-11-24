//! Notifications and alerts. Anything that wants people to know something calls [`emit`] with
//! an [`Event`]; rules (per team: event kinds to channels, optionally scoped to a project or
//! an environment, with a minimum severity) decide where it goes.
//!
//! The path of an event:
//!
//! 1. [`pipeline::route`] counts state changes of up/down pairs (flap detection), matches the
//!    rules, collapses repeats of the same key inside the dedup window into one stored event
//!    with a count, and queues one delivery per channel. During the team's quiet hours
//!    anything below critical is deferred instead.
//! 2. [`delivery::tick`] runs from the jobs dispatcher's loop (there is no service of its
//!    own): it sends what is due, retries failures with exponential backoff, parks what keeps
//!    failing as dead letters and holds each channel to a rate limit. Deferred deliveries leave
//!    as one digest per channel when quiet hours end, and an event that repeated inside its
//!    dedup window gets one "happened N times" follow-up.
//! 3. [`alerts::evaluate`] runs after every sampler sweep and turns metric alerts (CPU, memory,
//!    disk, GPU of a server; CPU or memory of a resource) into events and recoveries.
//!
//! Every event also lands in the bell's history ([`service::feed`]) when it is at least a
//! warning or some rule wants it. A channel's secrets are write-only: sealed with the secret
//! box, opened only to send.

pub mod alerts;
pub mod channel;
pub mod delivery;
pub mod event;
pub mod format;
pub mod hooks;
pub mod pipeline;
mod routes;
pub mod send;
pub mod service;
pub mod settings;
#[cfg(test)]
mod tests;

use sqlx::PgPool;
use time::OffsetDateTime;

pub use event::{Event, EventKind, Severity};
pub use routes::router;

use crate::state::AppState;

/// Routes an event. Never fails the caller: a problem is logged, because the thing being
/// reported matters more than the report of it.
pub async fn emit(state: &AppState, event: Event) {
	let kind = event.kind;
	match pipeline::route(&state.db, event, OffsetDateTime::now_utc()).await {
		Ok(pipeline::Outcome::Recorded { deliveries, .. }) if deliveries > 0 => {
			// Wake the dispatcher, which runs the delivery pass.
			state.jobs.notify.notify_one();
		}
		Ok(_) => {}
		Err(e) => tracing::warn!(
			kind = kind.as_str(),
			"cannot route a notification event: {e}"
		),
	}
}

/// [`emit`] for a caller that has only the database. The delivery pass picks the message up
/// on its next round (a few seconds) instead of being woken.
pub async fn emit_db(db: &PgPool, event: Event) {
	let kind = event.kind;
	if let Err(e) = pipeline::route(db, event, OffsetDateTime::now_utc()).await {
		tracing::warn!(
			kind = kind.as_str(),
			"cannot route a notification event: {e}"
		);
	}
}

/// Hourly housekeeping from the maintenance loop.
pub async fn maintain(db: &PgPool) {
	if let Err(e) = delivery::purge(db).await {
		tracing::warn!("failed to purge old notifications: {e}");
	}
}
