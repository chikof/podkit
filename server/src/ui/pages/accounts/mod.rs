//! Account pages: sign-up and creating a team.

use axum::Router;

use crate::state::AppState;

mod register;
mod teams;

/// The create-team form, as a page body; also shown to a user with no team.
pub use teams::create_form;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(teams::router())
		.merge(register::router())
}
