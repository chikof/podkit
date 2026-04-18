use std::sync::Arc;

use database::PgPool;
use tokio::net::TcpListener;
use tracing::info;

use crate::{auth::token::TokenService, config::ServerConfig, error::AppResult, routes::routes};

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
	better_tracing::fmt().init();

	let config = ServerConfig::load()?;

	info!(version = env!("CARGO_PKG_VERSION"), "podkit starting");

	let addr = format!("{}:{}", config.host, config.port);

	let routes = routes(&config).await?;
	let listener = TcpListener::bind(&addr).await?;

	info!("started server on http://{addr}");
	axum::serve(listener, routes).await?;

	Ok(())
}
