mod create;
mod delete;
mod list;

use axum::Router;
use axum::routing::{delete, post};

use crate::AppState;

pub use create::create_server;
pub use delete::delete_server;
pub use list::list_servers;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create_server).get(list_servers))
		.route("/{server_id}", delete(delete_server))
}
