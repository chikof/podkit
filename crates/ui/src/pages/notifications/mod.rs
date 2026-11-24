//! Notifications (Settings): the team's channels, rules, metric alerts, noise control and
//! delivery log on one page, and the bell's history on another. The actions answer
//! with a redirect back to the page; a refused one puts its message in the form's own error line.
//!
//! Secrets are write-only: the forms post them once and no page, signal or response carries
//! them back. A channel's card shows its host (`hint`) and the outcome of its last test, nothing
//! more.

mod history;
mod settings;

#[cfg(test)]
mod tests;

use axum::Router;

use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(settings::router())
		.merge(history::router())
}
