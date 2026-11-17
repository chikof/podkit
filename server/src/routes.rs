use axum::Router;
use axum::routing::get;
use tower_cookies::CookieManagerLayer;

use crate::error::AppResult;
use crate::state::AppState;
use crate::{
	applications, auth, backups, databases, deployments, domains, env_vars, members, projects,
	servers, teams, ui, volumes, webhooks,
};

const APP: &str = "/teams/{team_id}/projects/{project_id}/applications/{application_id}";
const DB: &str = "/teams/{team_id}/projects/{project_id}/databases";

async fn health() -> &'static str {
	"ok"
}

/// The JSON API and the web UI share one origin, so there is no CORS layer:
/// browsers can only call the API from podkit's own pages.
pub async fn routes(state: AppState) -> AppResult<Router> {
	Ok(Router::new()
		.merge(ui::router())
		.route("/health", get(health))
		.nest("/auth", auth::router())
		.nest("/teams", teams::router())
		.nest("/teams/{team_id}", members::router())
		.nest("/teams/{team_id}/projects", projects::router())
		.nest("/teams/{team_id}/servers", servers::router())
		.nest(
			"/teams/{team_id}/projects/{project_id}/applications",
			applications::router(),
		)
		.nest(&format!("{APP}/deployments"), deployments::router())
		.nest(&format!("{APP}/env-vars"), env_vars::router())
		.nest(&format!("{APP}/domains"), domains::router())
		.nest(&format!("{APP}/volumes"), volumes::router())
		.nest(
			"/teams/{team_id}/backup-targets",
			backups::targets::router(),
		)
		.nest(DB, databases::router())
		.nest(&format!("{DB}/{{database_id}}/backups"), backups::router())
		.nest("/webhooks", webhooks::router())
		.layer(CookieManagerLayer::new())
		.with_state(state))
}
