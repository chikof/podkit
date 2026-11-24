//! Account pages: sign-up, accepting an invite and creating a team.

use axum::Router;

use podkit_core::state::AppState;

mod invite;
mod register;
mod teams;

/// The create-team form, as a page body; also shown to a user with no team.
pub use teams::create_form;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(teams::router())
		.merge(register::router())
		.merge(invite::router())
}
