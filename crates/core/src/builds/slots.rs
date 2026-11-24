//! Build concurrency: how many builds a server runs at once. The rest wait, in the order they
//! came, and say so in the deployment log.
//!
//! The count lives in the control plane's memory, per server. That is enough because the
//! control plane is the only thing that starts builds; after a restart the jobs that were
//! running are failed or requeued and count again from zero.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::ids::ServerId;

struct Gate {
	running: usize,
	changed: std::sync::Arc<Notify>,
}

static GATES: LazyLock<Mutex<HashMap<ServerId, Gate>>> = LazyLock::new(Mutex::default);

/// A build's place on a server. Dropping it frees the place for the next build.
#[derive(Debug)]
pub struct Slot {
	server: ServerId,
}

impl Drop for Slot {
	fn drop(&mut self) {
		let mut gates = GATES
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if let Some(gate) = gates.get_mut(&self.server) {
			gate.running = gate.running.saturating_sub(1);
			gate.changed.notify_waiters();
		}
	}
}

/// Takes a place if the server has one under `limit`, else `None`.
pub fn try_take(server: ServerId, limit: usize) -> Option<Slot> {
	let mut gates = GATES
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let gate = gates.entry(server).or_insert_with(|| Gate {
		running: 0,
		changed: std::sync::Arc::new(Notify::new()),
	});
	if gate.running < limit.max(1) {
		gate.running += 1;
		Some(Slot { server })
	} else {
		None
	}
}

/// How many builds run on `server` now.
#[must_use]
pub fn running(server: ServerId) -> usize {
	GATES
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.get(&server)
		.map_or(0, |g| g.running)
}

/// Waits for a place on `server`, `limit` at a time. `None` when `cancel` fires first. Call
/// [`try_take`] first to know whether the build has to wait.
pub async fn acquire(server: ServerId, limit: usize, cancel: &CancellationToken) -> Option<Slot> {
	loop {
		// Listen before looking, so a place freed in between is not missed.
		let changed = {
			let mut gates = GATES
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			std::sync::Arc::clone(
				&gates
					.entry(server)
					.or_insert_with(|| Gate {
						running: 0,
						changed: std::sync::Arc::new(Notify::new()),
					})
					.changed,
			)
		};
		let notified = changed.notified();
		tokio::pin!(notified);
		notified.as_mut().enable();
		if let Some(slot) = try_take(server, limit) {
			return Some(slot);
		}
		tokio::select! {
			() = cancel.cancelled() => return None,
			() = notified => {}
		}
	}
}
