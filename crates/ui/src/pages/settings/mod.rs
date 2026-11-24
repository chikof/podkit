//! Team settings: members, the team itself, registration and appearance.

use axum::Router;

use podkit_core::state::AppState;

mod appearance;
mod audit;
mod authentication;
mod builds;
mod frame;
mod members;
mod registration;
mod tailscale;
mod team;
mod tokens;
mod usage;

/// The settings frame, for pages of other areas that sit among them (the Hugging Face token).
pub use audit::activity;
pub use frame::{settings_body, settings_crumbs};

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(team::router())
		.merge(members::router())
		.merge(registration::router())
		.merge(authentication::router())
		.merge(appearance::router())
		.merge(tokens::router())
		.merge(tailscale::router())
		.merge(audit::router())
		.merge(builds::router())
		.merge(usage::router())
}
