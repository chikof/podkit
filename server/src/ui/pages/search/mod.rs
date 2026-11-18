//! The command palette's search endpoint.

use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
}
