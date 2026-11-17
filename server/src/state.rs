//! Shared state handed to every route handler.

use std::path::PathBuf;
use std::sync::Arc;

use crypto::age::SecretBox;
use sqlx::PgPool;

use crate::agents::AgentPool;
use crate::jobs::worker::Jobs;
use crate::logs::LogHub;

/// Resolved runtime configuration routes and background tasks need.
#[derive(Debug, Clone)]
pub struct Config {
	/// Port Traefik listens on for plain HTTP ingress.
	pub ingress_port: u16,
	/// Port Traefik listens on for HTTPS ingress.
	pub https_port: u16,
	/// Contact email for ACME certs; `None` disables HTTPS/custom domains.
	pub acme_email: Option<String>,
	/// Control-plane state directory (job logs live in `logs/jobs`).
	pub data_dir: PathBuf,
	/// Unix socket of the local server's agent.
	pub local_agent_socket: PathBuf,
	/// Directory holding `podkit-agent[-{arch}]` binaries for bootstrap.
	pub agent_binaries_dir: PathBuf,
	/// Base URL of the Hugging Face hub; tests point it at a local stub.
	pub hf_endpoint: String,
	/// Replaces the per-engine readiness timeout of model starts; tests shorten it.
	pub model_ready_timeout: Option<std::time::Duration>,
	/// Pause between readiness probes of a starting model.
	pub model_ready_interval: std::time::Duration,
}

#[derive(Clone)]
pub struct AppState {
	pub db: PgPool,
	/// Encrypts secrets at rest (ssh keys, deploy keys, env var values).
	pub secrets: Arc<SecretBox>,
	pub config: Arc<Config>,
	pub agents: Arc<AgentPool>,
	pub jobs: Arc<Jobs>,
	pub logs: Arc<LogHub>,
	pub http: reqwest::Client,
	/// Cancelled on SIGINT/SIGTERM; long-lived streams end on it so graceful shutdown can drain.
	pub shutdown: tokio_util::sync::CancellationToken,
}
