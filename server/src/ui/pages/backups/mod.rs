//! Backups: the team's S3 destinations (`targets`) and the Backups tab of a database
//! (`history`: schedule, history, back up now, restore).

use axum::Router;

use crate::state::AppState;

mod history;
mod targets;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(targets::router())
		.merge(history::router())
}
