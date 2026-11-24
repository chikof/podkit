//! Shared state handed to every route handler.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crypto::age::SecretBox;
use sqlx::PgPool;

use crate::agents::AgentPool;
use crate::jobs::worker::Jobs;
use crate::logs::LogHub;

/// The HTTP client of [`AppState`], built when something first uses it.
///
/// `reqwest::Client::new()` loads the platform's root certificates, about 1 MB of heap that a
/// server with no outbound traffic (no importer, no webhook, no registry) never needs. Clones
/// share the one client.
#[derive(Clone, Default)]
pub struct Http(Arc<OnceLock<reqwest::Client>>);

impl std::ops::Deref for Http {
	type Target = reqwest::Client;

	fn deref(&self) -> &reqwest::Client {
		self.0.get_or_init(reqwest::Client::new)
	}
}

impl From<reqwest::Client> for Http {
	fn from(client: reqwest::Client) -> Self {
		Self(Arc::new(OnceLock::from(client)))
	}
}

/// Who may sign up through `POST /auth/register` and `/register`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Registration {
	/// Only while no account exists yet: the first user becomes the admin.
	#[default]
	FirstUser,
	/// Anyone may create an account.
	Open,
	/// Nobody may.
	Closed,
}

impl std::str::FromStr for Registration {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s.trim().to_ascii_lowercase().as_str() {
			"first-user" => Ok(Self::FirstUser),
			"open" => Ok(Self::Open),
			"closed" => Ok(Self::Closed),
			other => Err(format!(
				"invalid registration policy {other:?}; use first-user, open or closed"
			)),
		}
	}
}

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
	/// Who may create an account.
	pub registration: Registration,
	/// `OUTBOUND_PRIVATE`: `Some` pins whether private addresses are reachable, over the
	/// instance setting. `None` leaves it to the setting and the registration default.
	pub outbound_private: Option<bool>,
	/// The address git providers can reach podkit at (`PUBLIC_URL`), without a trailing slash.
	/// `None` on a homelab with no public address: push webhooks cannot be created then.
	pub public_url: Option<String>,
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
	/// The shared HTTP client; built on the first outbound request (see [`Http`]).
	pub http: Http,
	/// Every request to an address a person typed goes through this: see `net::outbound`.
	pub outbound: outbound::Outbound,
	/// The first-run setup token, armed at start while no account exists (`auth::setup`).
	pub setup: Arc<crate::auth::setup::SetupToken>,
	/// Cancelled on SIGINT/SIGTERM; long-lived streams end on it so graceful shutdown can drain.
	pub shutdown: tokio_util::sync::CancellationToken,
	/// The billing provider of this build (`cloud` feature; see `docs/cloud-extension.md`).
	#[cfg(feature = "cloud")]
	pub billing: crate::billing::Billing,
}
