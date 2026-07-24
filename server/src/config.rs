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

	#[field(env = "HOST", default = "0.0.0.0".into())]
	pub host: String,

	#[field(env = "PORT", default = 8080)]
	// detski NOTE: This thing gave me a lovely Os { code: 98, kind: AddrInUse, message: "Address already in use" }
	// chiko NOTE: should be fixed by now, ill leave the comment anyways in case it happens again.
	pub port: i32,
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
