//! Team settings: members, the team itself, registration and appearance.

use axum::Router;

use crate::state::AppState;

mod appearance;
mod frame;
mod members;
mod registration;
mod team;

/// The tab row of the settings pages, for pages of other areas that sit among them (the
/// Hugging Face token).
pub use frame::settings_tabs;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(team::router())
		.merge(members::router())
		.merge(registration::router())
		.merge(appearance::router())
}
