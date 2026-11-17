//! Periodic housekeeping that runs for the life of the process.

use std::time::Duration;

use crate::auth::session;
use crate::state::AppState;

pub fn spawn(state: AppState) {
	tokio::spawn(async move {
		let mut interval = tokio::time::interval(Duration::from_hours(1));
		loop {
			interval.tick().await;
			if let Err(e) = session::purge_expired(&state.db).await {
				tracing::warn!("failed to purge expired sessions: {e}");
			}
		}
	});
}
