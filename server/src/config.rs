use forgeconf::forgeconf;

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

#[cfg_attr(feature = "config_file", forgeconf(config(path = config_path())))]
#[cfg_attr(not(feature = "config_file"), forgeconf)]
pub struct ServerConfig {
	#[field(env = "DATABASE_URL")]
	pub database_url: String,

	#[field(env = "JWT_SECRET")]
	pub jwt_secret: String,

	#[field(env = "AGE_SECRET_KEY")]
	pub age_secret_key: String,

	#[field(env = "HOST", default = "0.0.0.0".into())]
	pub host: String,

	#[field(env = "PORT", default = 8080)]
	// detski NOTE: This thing gave me a lovely Os { code: 98, kind: AddrInUse, message: "Address already in use" }
	// chiko NOTE: should be fixed by now, ill leave the comment anyways in case it happens again.
	pub port: i32,

	/// Host port each server's Traefik instance publishes for HTTP ingress.
	/// Defaults to 80; override for rootless podman hosts that haven't
	/// lowered `net.ipv4.ip_unprivileged_port_start` (or aren't running
	/// rootful podman) and so can't bind privileged ports.
	#[field(env = "INGRESS_PORT", default = 80)]
	pub ingress_port: i32,

	/// Host port each server's Traefik instance publishes for HTTPS.
	/// Only opened when `acme_email` is set.
	#[field(env = "HTTPS_PORT", default = 443)]
	pub https_port: i32,

	/// Enables Traefik's Let's Encrypt certificate resolver for custom
	/// domains when set; `None` disables ACME entirely (no `websecure`
	/// entrypoint opened, no HTTPS routers created).
	#[field(env = "ACME_EMAIL", optional = true)]
	pub acme_email: Option<String>,

	/// Comma-separated list of origins the dashboard (or any other browser
	/// client) is served from; reflected as `Access-Control-Allow-Origin`
	/// on the API's CORS layer. Defaults cover the `SvelteKit` dev/preview
	/// servers (`vite dev` on 5173, `vite preview` on 4173).
	#[field(
		env = "DASHBOARD_ORIGINS",
		default = "http://localhost:5173,http://localhost:4173".into()
	)]
	pub dashboard_origins: String,
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
