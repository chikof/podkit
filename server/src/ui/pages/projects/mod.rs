//! Projects and what lives in them: the projects grid, an environment's resource cards and
//! the create-resource picker, then databases. Applications have their own area, `apps`.

use axum::Router;

use crate::state::AppState;

// The application pages reuse the resource status line and relative times.
pub(in crate::ui) mod cards;
// The backup pages build on the database page frame.
pub(in crate::ui) mod databases;
mod environment;
mod list;
mod picker;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(list::router())
		.merge(environment::router())
		.merge(databases::router())
}
