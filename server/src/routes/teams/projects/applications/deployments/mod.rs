mod create;
mod get;
mod list;
mod logs;
mod rollback;

use axum::Router;
use axum::routing::{get, post};

use crate::AppState;

pub use create::create_deployment;
pub use get::get_deployment;
pub use list::list_deployments;
pub use logs::get_deployment_logs;
pub use rollback::rollback_deployment;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create_deployment).get(list_deployments))
		.route("/{deployment_id}", get(get_deployment))
		.route("/{deployment_id}/rollback", post(rollback_deployment))
		.route("/{deployment_id}/logs", get(get_deployment_logs))
}
