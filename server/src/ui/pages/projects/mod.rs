//! Projects and what lives in them: the projects tiles, an environment's resource rows and
//! the create-resource picker page. Applications and databases have their own areas.

use axum::Router;

use crate::state::AppState;

// The application pages reuse the resource status line and relative times.
pub(in crate::ui) mod cards;
mod environment;
mod list;
mod picker;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(list::router())
		.merge(environment::router())
}
