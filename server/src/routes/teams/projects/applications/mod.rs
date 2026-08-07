mod create;
mod delete;
mod deployments;
mod domains;
mod env_vars;
mod list;
mod restart;
mod stop;
mod update;

use axum::Router;
use axum::routing::{delete, post};

use crate::AppState;

pub use create::create_application;
pub use delete::delete_application;
pub use list::list_applications;
pub use restart::restart_application;
pub use stop::stop_application;
pub use update::update_application;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create_application).get(list_applications))
		.route(
			"/{application_id}",
			delete(delete_application).patch(update_application),
		)
		.route("/{application_id}/stop", post(stop_application))
		.route("/{application_id}/restart", post(restart_application))
		.nest("/{application_id}/deployments", deployments::router())
		.nest("/{application_id}/env-vars", env_vars::router())
		.nest("/{application_id}/domains", domains::router())
}
