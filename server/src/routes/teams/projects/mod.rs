mod create;
mod delete;
mod list;

use axum::Router;
use axum::routing::{delete, post};

use crate::AppState;

pub use create::create_project;
pub use delete::delete_project;
pub use list::list_projects;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create_project).get(list_projects))
		.route("/{project_id}", delete(delete_project))
}
