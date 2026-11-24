use forgeconf::Config;

use crate::error::ServerError;

#[allow(dead_code)]
#[cfg(feature = "config_file")]
pub fn config_path() -> String {
	let dir = dirs::config_dir()
		.unwrap_or(std::path::PathBuf::from("."))
		.join("podkit");
	let config_name = "config.toml";

	dir.join(config_name)
		.to_str()
		.unwrap_or(config_name)
		.to_string()
}

#[derive(Config)]
#[cfg_attr(feature = "config_file", config(file(path = config_path())))]
pub struct ServerConfig {
	#[config(env = "DATABASE_URL")]
	pub database_url: String,

	#[config(env = "AGE_SECRET_KEY")]
	pub age_secret_key: String,

	#[config(env = "HOST", default = "0.0.0.0")]
	pub host: String,

	#[config(env = "PORT", default = 8080)]
	// detski NOTE: This thing gave me a lovely Os { code: 98, kind: AddrInUse, message: "Address already in use" }
	// chiko NOTE: should be fixed by now, ill leave the comment anyways in case it happens again.
	pub port: i32,

	/// Host port each server's Traefik instance publishes for HTTP ingress.
	/// Defaults to 80; override for rootless podman hosts that haven't
	/// lowered `net.ipv4.ip_unprivileged_port_start` (or aren't running
	/// rootful podman) and so can't bind privileged ports.
	#[config(env = "INGRESS_PORT", default = 80)]
	pub ingress_port: i32,

	/// Host port each server's Traefik instance publishes for HTTPS.
	/// Only opened when `acme_email` is set.
	#[config(env = "HTTPS_PORT", default = 443)]
	pub https_port: i32,

	/// Enables Traefik's Let's Encrypt certificate resolver for custom
	/// domains when set; `None` disables ACME entirely (no `websecure`
	/// entrypoint opened, no HTTPS routers created).
	#[config(env = "ACME_EMAIL")]
	pub acme_email: Option<String>,

	/// Control-plane state directory; defaults to `dirs::data_dir()/podkit`.
	#[config(env = "DATA_DIR")]
	pub data_dir: Option<String>,

	/// Unix socket of the local server's agent; defaults to
	/// `$HOME/.podkit/agent.sock`.
	#[config(env = "LOCAL_AGENT_SOCKET")]
	pub local_agent_socket: Option<String>,

	/// Directory with `podkit-agent[-{arch}]` binaries uploaded during server
	/// bootstrap; defaults to the directory of the running executable.
	#[config(env = "AGENT_BINARIES_DIR")]
	pub agent_binaries_dir: Option<String>,

	/// Base URL of the Hugging Face hub used to size model downloads;
	/// defaults to `https://huggingface.co`.
	#[config(env = "HF_ENDPOINT")]
	pub hf_endpoint: Option<String>,

	/// The address git providers (GitHub, GitLab, Gitea) reach podkit at, like
	/// `https://podkit.example.com`. Push webhooks need it; without it deploys are manual.
	#[config(env = "PUBLIC_URL")]
	pub public_url: Option<String>,

	/// Reverse proxies whose `X-Forwarded-For` / `Forwarded` headers are believed:
	/// addresses or CIDR ranges, comma-separated (`127.0.0.1, 172.18.0.0/16`). Empty by
	/// default, so the client address is always the socket peer's.
	#[config(env = "TRUSTED_PROXIES")]
	pub trusted_proxies: Option<String>,

	/// Who may create an account: `first-user` (only while no account
	/// exists; the default, for a homelab where the first user is the
	/// admin), `open` (anyone; for a hosted instance) or `closed` (nobody).
	#[config(env = "REGISTRATION", default = "first-user")]
	pub registration: String,

	/// Whether podkit may make requests to private network addresses (RFC 1918, `fc00::/7`,
	/// CGNAT) on behalf of notification channels, importers, uptime checks, git providers,
	/// identity providers and backup targets: `allow` or `deny`. Unset, the instance admin's
	/// setting decides, and without one `first-user` and `closed` instances allow it (a
	/// homelab) while `open` ones deny it. Loopback, link-local and metadata addresses are
	/// never reachable.
	#[config(env = "OUTBOUND_PRIVATE")]
	pub outbound_private: Option<String>,
	/// Seconds between the samples of each active server's CPU, memory, disk,
	/// network and containers (`/v1/stats`). At least 5.
	#[config(env = "METRICS_INTERVAL_SECS", default = 30)]
	pub metrics_interval_secs: i32,
}

impl ServerConfig {
	pub fn load() -> Result<Self, ServerError> {
		Ok(Self::loader().load()?)
	}

	#[cfg(feature = "config_file")]
	pub fn create_if_missing() -> std::io::Result<()> {
		let path = std::path::PathBuf::from(config_path());
		let dir = path.parent().unwrap();

		if !dir.exists() {
			std::fs::create_dir_all(dir)?;
		}

		if !path.exists() {
			std::fs::write(&path, include_str!("../../config.default.toml"))?;
		}

		Ok(())
	}
}
