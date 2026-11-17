//! Shared state handed to every route handler.

use std::sync::Arc;

use crypto::age::SecretBox;
use sqlx::PgPool;

/// Resolved runtime configuration routes and background tasks need.
#[derive(Debug, Clone)]
pub struct Config {
	/// Port Traefik listens on for plain HTTP ingress.
	pub ingress_port: u16,
	/// Port Traefik listens on for HTTPS ingress.
	pub https_port: u16,
	/// Contact email for ACME certs; `None` disables HTTPS/custom domains.
	pub acme_email: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
	pub db: PgPool,
	/// Encrypts secrets at rest (ssh keys, deploy keys, env var values).
	pub secrets: Arc<SecretBox>,
	pub config: Arc<Config>,
}
