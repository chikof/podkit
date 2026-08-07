use axum::Router;
use axum::http::{HeaderValue, Method, header};
use axum::routing::{get, post};
use tower_cookies::CookieManagerLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

mod auth;
mod teams;
mod webhooks;

use crate::AppState;
use crate::error::AppResult;

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
			Method::PATCH,
			Method::DELETE,
			Method::OPTIONS,
		])
		.allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

	Ok(Router::new()
		.route("/health", get(health))
		.route("/auth/register", post(auth::register))
		.route("/auth/login", post(auth::login))
		.route("/auth/logout", post(auth::logout))
		.nest("/teams", teams::router())
		.nest("/webhooks", webhooks::router())
		.layer(CookieManagerLayer::new())
		.layer(cors)
		.with_state(state))
}
