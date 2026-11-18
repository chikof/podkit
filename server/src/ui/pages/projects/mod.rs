//! Projects and what lives in them: applications, deployments, application settings and
//! databases.

use axum::Router;

use crate::state::AppState;

mod app_config;
mod apps;
pub(in crate::ui) use apps::app_url;
// The backup pages build on the database page frame.
pub(in crate::ui) mod databases;
mod deployments;
mod project;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(project::router())
		.merge(apps::router())
		.merge(deployments::router())
		.merge(app_config::router())
		.merge(databases::router())
}
