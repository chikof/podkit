use std::fs;

use forgeconf::forgeconf;

use crate::error::ServerError;

pub fn config_path() -> String {
	let dir = dirs::config_dir()
		.unwrap_or_else(|| std::path::PathBuf::from("."))
		.join("podkit");
	let config_name = "config.toml";

	dir.join(config_name)
		.to_str()
		.unwrap_or(config_name)
		.to_string()
}

#[forgeconf(config(path = config_path()))]
pub struct ServerConfig {
	#[field(env = "DATABASE_URL")]
	pub database_url: String,

	#[field(env = "JWT_SECRET")]
	pub jwt_secret: String,

	#[field(env = "HOST", default = "0.0.0.0".into())]
	pub host: String,

	#[field(env = "PORT", default = 8080)]
	pub port: i32,
}

impl ServerConfig {
	pub fn load() -> Result<Self, ServerError> {
		Ok(Self::loader().load()?)
	}

	pub fn create_if_missing() -> std::io::Result<()> {
		let path = std::path::PathBuf::from(config_path());
		let dir = path.parent().unwrap();

		if !dir.exists() {
			fs::create_dir_all(dir)?;
		}

		if !path.exists() {
			fs::write(&path, include_str!("../../config.default.toml"))?;
		}

		Ok(())
	}
}
