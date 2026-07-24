use axum::Router;
use axum::routing::{get, post};
use tower_cookies::CookieManagerLayer;

mod auth;
mod teams;

use crate::AppState;
use crate::error::AppResult;

async fn health() -> &'static str {
	"ok"
}

pub async fn routes(state: AppState) -> AppResult<Router> {
	Ok(Router::new()
		.route("/health", get(health))
		.route("/auth/register", post(auth::register))
		.route("/auth/login", post(auth::login))
		.route("/auth/logout", post(auth::logout))
		.nest("/teams", teams::router())
		.layer(CookieManagerLayer::new())
		.with_state(state))
}
