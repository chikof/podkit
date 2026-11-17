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

	/// Comma-separated list of origins the dashboard (or any other browser
	/// client) is served from; reflected as `Access-Control-Allow-Origin`
	/// on the API's CORS layer. Defaults cover the `SvelteKit` dev/preview
	/// servers (`vite dev` on 5173, `vite preview` on 4173).
	#[config(
		env = "DASHBOARD_ORIGINS",
		default = "http://localhost:5173,http://localhost:4173"
	)]
	pub dashboard_origins: String,

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
