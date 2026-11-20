//! podkit's control plane: auth, team/project/application management, and
//! the durable job queue that drives each server's `podkit-agent`.

use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crypto::age::SecretBox;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agents::AgentPool;
use crate::config::ServerConfig;
use crate::error::{AppResult, ServerError};
use crate::jobs::worker::{self, Jobs};
use crate::logs::LogHub;
use crate::routes::routes;
use crate::state::{AppState, Config};

mod agents;
mod applications;
mod auth;
mod backups;
mod catalog;
mod config;
mod databases;
mod db;
mod deployments;
mod domains;
mod env_vars;
mod environments;
mod error;
mod ids;
mod jobs;
mod links;
mod logos;
mod logs;
mod maintenance;
mod members;
mod metrics;
mod model_files;
mod models;
mod projects;
mod routes;
mod scanning;
mod scope;
mod search;
mod servers;
mod stacks;
mod state;
mod teams;
mod templates;
mod ui;
mod validate;
mod volumes;
mod webhooks;

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> AppResult<()> {
	if let Some(code) = templates::import::cli() {
		std::process::exit(code);
	}
	#[cfg(debug_assertions)]
	dotenvy::dotenv().ok(); // I wasn’t really sure whether we should use dotenv_override() or not

	better_tracing::fmt().init();
	if rustls::crypto::ring::default_provider()
		.install_default()
		.is_err()
	{
		warn!("a rustls crypto provider was already installed");
	}

	#[cfg(feature = "config_file")]
	ServerConfig::create_if_missing()?;

	let config = ServerConfig::load()?;
	info!(version = env!("CARGO_PKG_VERSION"), "podkit starting");

	let secrets = Arc::new(
		SecretBox::from_identity_str(&config.age_secret_key)
			.map_err(|e| ServerError::InvalidSecretKey(e.to_string()))?,
	);

	let runtime_config = resolve_config(&config)?;
	let log_dir = runtime_config.data_dir.join("logs").join("jobs");
	std::fs::DirBuilder::new()
		.recursive(true)
		.mode(0o700)
		.create(&log_dir)?;

	let addr = format!("{}:{}", config.host, config.port);
	let db = db::connect(&config.database_url).await?;
	db::migrate(&db)
		.await
		.map_err(|e| ServerError::Sqlx(e.into()))?;

	let shutdown = CancellationToken::new();
	let state = AppState {
		db,
		secrets,
		agents: Arc::new(AgentPool::new(runtime_config.local_agent_socket.clone())),
		jobs: Arc::new(Jobs::new()),
		logs: Arc::new(LogHub::new(log_dir)),
		http: reqwest::Client::new(),
		shutdown: shutdown.clone(),
		config: Arc::new(runtime_config),
	};

	worker::recover(&state).await?;
	if let Err(e) = servers::bootstrap::enqueue_outdated(&state).await {
		warn!("failed to enqueue agent upgrades: {e}");
	}

	let dispatcher = tokio::spawn(worker::run_dispatcher(state.clone(), shutdown.clone()));
	tokio::spawn(servers::sync::run(state.clone(), shutdown.clone()));
	tokio::spawn(metrics::sampler::run(
		state.clone(),
		std::time::Duration::from_secs(
			u64::try_from(config.metrics_interval_secs.max(5)).unwrap_or(30),
		),
		shutdown.clone(),
	));
	tokio::spawn(backups::schedule::run(state.clone(), shutdown.clone()));
	maintenance::spawn(state.clone());
	tokio::spawn(signal_shutdown(shutdown.clone()));

	let app = routes(state.clone()).await?;
	let listener = TcpListener::bind(&addr).await?;

	info!("started http server on http://{addr}");
	axum::serve(listener, app)
		.with_graceful_shutdown(shutdown.clone().cancelled_owned())
		.await?;

	if let Err(e) = dispatcher.await {
		warn!("job dispatcher panicked: {e}");
	}
	state.jobs.tracker.close();
	if tokio::time::timeout(Duration::from_secs(30), state.jobs.tracker.wait())
		.await
		.is_err()
	{
		warn!("jobs still running after 30s; exiting anyway");
	}

	Ok(())
}

fn resolve_config(config: &ServerConfig) -> AppResult<Config> {
	let home = || dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
	let data_dir = config.data_dir.as_ref().map_or_else(
		|| {
			dirs::data_dir()
				.unwrap_or_else(|| home().join(".local/share"))
				.join("podkit")
		},
		PathBuf::from,
	);
	let local_agent_socket = config
		.local_agent_socket
		.as_ref()
		.map_or_else(|| home().join(".podkit").join("agent.sock"), PathBuf::from);
	let agent_binaries_dir = match &config.agent_binaries_dir {
		Some(dir) => PathBuf::from(dir),
		None => std::env::current_exe()?
			.parent()
			.map(PathBuf::from)
			.unwrap_or_default(),
	};
	let registration = config
		.registration
		.parse()
		.map_err(ServerError::Validation)?;
	Ok(Config {
		registration,
		ingress_port: u16::try_from(config.ingress_port).unwrap_or(80),
		https_port: u16::try_from(config.https_port).unwrap_or(443),
		acme_email: config.acme_email.clone(),
		data_dir,
		local_agent_socket,
		agent_binaries_dir,
		hf_endpoint: config
			.hf_endpoint
			.as_deref()
			.map_or("https://huggingface.co", |e| e.trim_end_matches('/'))
			.to_owned(),
		model_ready_timeout: None,
		model_ready_interval: Duration::from_secs(3),
	})
}

async fn signal_shutdown(token: CancellationToken) {
	let ctrl_c = tokio::signal::ctrl_c();
	let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
		Ok(s) => s,
		Err(e) => {
			warn!("cannot listen for SIGTERM: {e}");
			let _ = ctrl_c.await;
			token.cancel();
			return;
		}
	};
	tokio::select! {
		_ = ctrl_c => {}
		_ = term.recv() => {}
	}
	info!("shutting down");
	token.cancel();
}
