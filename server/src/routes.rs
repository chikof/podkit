use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use database::connection::get_db_connection;
use tower_cookies::CookieManagerLayer;

mod auth;

use crate::auth::token::TokenService;
use crate::config::ServerConfig;
use crate::error::AppResult;
use crate::AppState;

async fn health() -> &'static str {
	"ok"
}

pub async fn routes(config: &ServerConfig) -> AppResult<Router> {
	let state = AppState {
		tokens: Arc::new(TokenService::new(config.jwt_secret.as_bytes())),
		pool: get_db_connection(Some(&config.database_url)).await?,
	};

	Ok(Router::new()
		.route("/health", get(health))
		.route("/auth/login", post(auth::login))
        .layer(CookieManagerLayer::new())
		.with_state(state))
}
