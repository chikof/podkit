use axum::Router;
use axum::http::{HeaderValue, Method, header};
use axum::routing::get;
use tower_cookies::CookieManagerLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::error::AppResult;
use crate::state::AppState;
use crate::{
	applications, auth, backups, databases, deployments, domains, env_vars, members, projects,
	servers, teams, volumes, webhooks,
};

const APP: &str = "/teams/{team_id}/projects/{project_id}/applications/{application_id}";
const DB: &str = "/teams/{team_id}/projects/{project_id}/databases";

async fn health() -> &'static str {
	"ok"
}

/// `dashboard_origins` is the comma-separated `DASHBOARD_ORIGINS` config
/// value. The dashboard authenticates with a bearer token (not cookies),
/// so this intentionally does not enable `allow_credentials`.
pub async fn routes(state: AppState, dashboard_origins: &str) -> AppResult<Router> {
	let origins: Vec<HeaderValue> = dashboard_origins
		.split(',')
		.map(str::trim)
		.filter(|o| !o.is_empty())
		.filter_map(|o| HeaderValue::from_str(o).ok())
		.collect();

	let cors = CorsLayer::new()
		.allow_origin(AllowOrigin::list(origins))
		.allow_methods([
			Method::GET,
			Method::POST,
			Method::PUT,
			Method::PATCH,
			Method::DELETE,
			Method::OPTIONS,
		])
		.allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

	Ok(Router::new()
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
		.layer(cors)
		.with_state(state))
}
