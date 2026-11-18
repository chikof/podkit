//! Servers: add, bootstrap, host key, delete.

use axum::Router;

use crate::state::AppState;

mod server;

pub fn router() -> Router<AppState> {
	server::router()
}
