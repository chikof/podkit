mod create;
mod delete;
mod list;

use axum::Router;
use axum::routing::{delete, post};

use crate::AppState;

pub use create::upsert_env_var;
pub use delete::delete_env_var;
pub use list::list_env_vars;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(upsert_env_var).get(list_env_vars))
		.route("/{env_var_id}", delete(delete_env_var))
}
