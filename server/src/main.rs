use std::sync::Arc;

use database::connection::{get_db_connection, migrate};
use database::models::token_revocations::TokenRevocation;
use database::PgPool;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::auth::token::TokenService;
use crate::config::ServerConfig;
use crate::error::AppResult;
use crate::routes::routes;

mod auth;
mod config;
mod error;
mod routes;

#[derive(Clone)]
pub struct AppState {
	pub tokens: Arc<TokenService>,
	pub pool: &'static PgPool,
}

#[tokio::main]
async fn main() -> AppResult<()> {

	#[cfg(debug_assertions)]
	dotenvy::dotenv().ok(); // I wasn’t really sure whether we should use dotenv_override() or not

	better_tracing::fmt().init();

	let config = ServerConfig::load()?;
	info!(version = env!("CARGO_PKG_VERSION"), "podkit starting");

	let addr = format!("{}:{}", config.host, config.port);
	let state = AppState {
		tokens: Arc::new(TokenService::new(config.jwt_secret.as_bytes())),
		pool: get_db_connection(Some(&config.database_url)).await?,
	};

	// run db migrations
	migrate().await?;
	clean_expired_tokens(state.pool);

	let routes = routes(state).await?;
	let listener = TcpListener::bind(&addr).await?;

	info!("started http server on http://{addr}");
	axum::serve(listener, routes).await?;

	Ok(())
}

// Runs every hour and cleans up expired revocation rows
fn clean_expired_tokens(pool: &'static PgPool) {
	tokio::spawn(async move {
		let mut interval = tokio::time::interval(std::time::Duration::from_hours(1));
		loop {
			interval.tick().await;
			if let Err(e) = TokenRevocation::purge_expired(pool).await {
				warn!("failed to purge expired token revocations: {e}");
			}
		}
	});
}
