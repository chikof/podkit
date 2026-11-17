//! podkit's API server: auth, team/project/application management, the
//! deploy pipeline, and the background health monitor that restarts
//! crashed containers.

use std::sync::Arc;

use crypto::age::SecretBox;
use tokio::net::TcpListener;
use tracing::info;

use crate::config::ServerConfig;
use crate::error::{AppResult, ServerError};
use crate::routes::routes;
use crate::state::{AppState, Config};

mod applications;
mod auth;
mod config;
mod db;
mod deploy_pipeline;
mod deployments;
mod domains;
mod env_vars;
mod error;
mod health_monitor;
mod ids;
mod maintenance;
mod members;
mod projects;
mod routes;
mod scope;
mod servers;
mod state;
mod teams;
mod validate;
mod webhooks;

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> AppResult<()> {
	#[cfg(debug_assertions)]
	dotenvy::dotenv().ok(); // I wasn’t really sure whether we should use dotenv_override() or not

	better_tracing::fmt().init();

	#[cfg(feature = "config_file")]
	ServerConfig::create_if_missing()?;

	let config = ServerConfig::load()?;
	info!(version = env!("CARGO_PKG_VERSION"), "podkit starting");

	let secrets = Arc::new(
		SecretBox::from_identity_str(&config.age_secret_key)
			.map_err(|e| ServerError::InvalidSecretKey(e.to_string()))?,
	);

	let addr = format!("{}:{}", config.host, config.port);
	let db = db::connect(&config.database_url).await?;
	db::migrate(&db)
		.await
		.map_err(|e| ServerError::Sqlx(e.into()))?;

	let state = AppState {
		db,
		secrets,
		config: Arc::new(Config {
			ingress_port: u16::try_from(config.ingress_port).unwrap_or(80),
			https_port: u16::try_from(config.https_port).unwrap_or(443),
			acme_email: config.acme_email.clone(),
		}),
	};

	maintenance::spawn(state.clone());
	health_monitor::spawn(state.clone());

	let routes = routes(state, &config.dashboard_origins).await?;
	let listener = TcpListener::bind(&addr).await?;

	info!("started http server on http://{addr}");
	axum::serve(listener, routes).await?;

	Ok(())
}
