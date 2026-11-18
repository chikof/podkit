//! Projects and what lives in them: the projects grid, an environment's resource cards and
//! the create-resource picker, then applications, deployments, application settings and
//! databases.

use axum::Router;

use crate::state::AppState;

mod app_config;
mod apps;
pub(in crate::ui) use apps::app_url;
mod cards;
// The backup pages build on the database page frame.
pub(in crate::ui) mod databases;
mod deployments;
mod environment;
mod list;
mod picker;
#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(list::router())
		.merge(environment::router())
		.merge(apps::router())
		.merge(deployments::router())
		.merge(app_config::router())
		.merge(databases::router())
}
