mod create;
mod delete;
mod list;

use axum::Router;
use axum::routing::{delete, post};

use crate::AppState;

pub use create::create_domain;
pub use delete::delete_domain;
pub use list::list_domains;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create_domain).get(list_domains))
		.route("/{domain_id}", delete(delete_domain))
}
