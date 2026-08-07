mod create;
mod list;
mod members;
mod projects;
mod roles;
mod servers;

use axum::Router;
use axum::routing::{get, patch, post};

use crate::AppState;

pub use create::create_team;
pub use list::list_teams;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create_team).get(list_teams))
		.route(
			"/{team_id}/members",
			get(members::list_members).post(members::add_member),
		)
		.route(
			"/{team_id}/members/{user_id}",
			patch(members::update_member_role),
		)
		.route("/{team_id}/roles", get(roles::list_roles))
		.nest("/{team_id}/projects", projects::router())
		.nest("/{team_id}/servers", servers::router())
}
