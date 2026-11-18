//! Team settings: members, the team itself, registration and appearance.

use axum::Router;

use crate::state::AppState;

mod members;

pub fn router() -> Router<AppState> {
	members::router()
}
