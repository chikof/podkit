//! Backup targets (S3 destinations) and the backups of a database.

use axum::Router;

use crate::state::AppState;

mod targets;

pub fn router() -> Router<AppState> {
	targets::router()
}
