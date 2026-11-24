//! The supported database engines: versions, images, data paths and the
//! container environment each one needs.

use proto::Engine;

use super::Credentials;
use crate::error::ServerError;

/// Everything podkit needs to know to run one engine. The first entry of
/// `versions` is the default.
pub struct EngineSpec {
	pub engine: Engine,
	pub versions: &'static [&'static str],
	image_prefix: &'static str,
	image_suffix: &'static str,
	data_path: fn(&str) -> &'static str,
	/// Port the engine listens on inside the container.
	pub port: u16,
	env: fn(&Credentials) -> Vec<(String, String)>,
	pub command: Option<&'static [&'static str]>,
}

impl EngineSpec {
	#[must_use]
	pub fn default_version(&self) -> &'static str {
		self.versions[0]
	}

	/// Resolves the requested version, defaulting to the first one.
	///
	/// # Errors
	/// `Validation` when `version` is not offered for this engine.
	pub fn resolve_version(&self, version: Option<&str>) -> Result<&'static str, ServerError> {
		let Some(version) = version else {
			return Ok(self.default_version());
		};
		self.versions
			.iter()
			.find(|v| **v == version)
			.copied()
			.ok_or_else(|| {
				ServerError::Validation(format!(
					"unsupported version {version}; available: {}",
					self.versions.join(", ")
				))
			})
	}

	#[must_use]
	pub fn image(&self, version: &str) -> String {
		format!("{}{version}{}", self.image_prefix, self.image_suffix)
	}

	#[must_use]
	pub fn data_path(&self, version: &str) -> &'static str {
		(self.data_path)(version)
	}

	#[must_use]
	pub fn env(&self, credentials: &Credentials) -> Vec<(String, String)> {
		(self.env)(credentials)
	}

	/// Connection url for `host:port`. Passwords are hex tokens, so nothing
	/// needs percent-encoding.
	#[must_use]
	pub fn url(&self, host: &str, port: u16, c: &Credentials) -> String {
		let Credentials {
			username,
			password,
			database,
		} = c;
		match self.engine {
			Engine::Postgres => {
				format!("postgres://{username}:{password}@{host}:{port}/{database}")
			}
			Engine::Mysql => format!("mysql://{username}:{password}@{host}:{port}/{database}"),
			Engine::Mongodb => {
				format!("mongodb://{username}:{password}@{host}:{port}/{database}?authSource=admin")
			}
			Engine::Redis => format!("redis://:{password}@{host}:{port}"),
		}
	}
}

fn postgres_data_path(version: &str) -> &'static str {
	// Postgres 18 images moved PGDATA to a versioned directory under this mount.
	if version == "17" {
		"/var/lib/postgresql/data"
	} else {
		"/var/lib/postgresql"
	}
}

fn postgres_env(c: &Credentials) -> Vec<(String, String)> {
	vec![
		("POSTGRES_USER".to_owned(), c.username.clone()),
		("POSTGRES_PASSWORD".to_owned(), c.password.clone()),
		("POSTGRES_DB".to_owned(), c.database.clone()),
	]
}

fn mysql_env(c: &Credentials) -> Vec<(String, String)> {
	vec![
		("MYSQL_ROOT_PASSWORD".to_owned(), c.password.clone()),
		("MYSQL_USER".to_owned(), c.username.clone()),
		("MYSQL_PASSWORD".to_owned(), c.password.clone()),
		("MYSQL_DATABASE".to_owned(), c.database.clone()),
	]
}

fn mongodb_env(c: &Credentials) -> Vec<(String, String)> {
	vec![
		("MONGO_INITDB_ROOT_USERNAME".to_owned(), c.username.clone()),
		("MONGO_INITDB_ROOT_PASSWORD".to_owned(), c.password.clone()),
		("MONGO_INITDB_DATABASE".to_owned(), c.database.clone()),
	]
}

fn redis_env(c: &Credentials) -> Vec<(String, String)> {
	vec![("REDIS_PASSWORD".to_owned(), c.password.clone())]
}

const POSTGRES: EngineSpec = EngineSpec {
	engine: Engine::Postgres,
	versions: &["18", "17"],
	image_prefix: "docker.io/library/postgres:",
	image_suffix: "-alpine",
	data_path: postgres_data_path,
	port: 5432,
	env: postgres_env,
	command: None,
};

const MYSQL: EngineSpec = EngineSpec {
	engine: Engine::Mysql,
	versions: &["9.7"],
	image_prefix: "docker.io/library/mysql:",
	image_suffix: "",
	data_path: |_| "/var/lib/mysql",
	port: 3306,
	env: mysql_env,
	command: None,
};

const MONGODB: EngineSpec = EngineSpec {
	engine: Engine::Mongodb,
	versions: &["9.0", "8.0"],
	image_prefix: "docker.io/library/mongo:",
	image_suffix: "",
	data_path: |_| "/data/db",
	port: 27017,
	env: mongodb_env,
	command: None,
};

const REDIS: EngineSpec = EngineSpec {
	engine: Engine::Redis,
	versions: &["8"],
	image_prefix: "docker.io/library/redis:",
	image_suffix: "-alpine",
	data_path: |_| "/data",
	port: 6379,
	env: redis_env,
	command: Some(&[
		"sh",
		"-c",
		r#"exec redis-server --requirepass "$REDIS_PASSWORD" --appendonly yes"#,
	]),
};

#[must_use]
pub fn spec(engine: Engine) -> &'static EngineSpec {
	match engine {
		Engine::Postgres => &POSTGRES,
		Engine::Mysql => &MYSQL,
		Engine::Mongodb => &MONGODB,
		Engine::Redis => &REDIS,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn credentials() -> Credentials {
		Credentials {
			username: "podkit".to_owned(),
			password: "pw".to_owned(),
			database: "podkit".to_owned(),
		}
	}

	#[test]
	fn first_version_is_the_default() {
		assert_eq!(spec(Engine::Postgres).default_version(), "18");
		assert_eq!(spec(Engine::Mongodb).default_version(), "9.0");
		assert_eq!(spec(Engine::Redis).resolve_version(None).unwrap(), "8");
	}

	#[test]
	fn unknown_versions_are_rejected() {
		assert_eq!(
			spec(Engine::Postgres).resolve_version(Some("17")).unwrap(),
			"17"
		);
		let err = spec(Engine::Postgres)
			.resolve_version(Some("16"))
			.unwrap_err();
		assert!(matches!(err, ServerError::Validation(m) if m.contains("18, 17")));
		assert!(spec(Engine::Mysql).resolve_version(Some("8.0")).is_err());
	}

	#[test]
	fn postgres_data_path_depends_on_version() {
		let postgres = spec(Engine::Postgres);
		assert_eq!(postgres.data_path("18"), "/var/lib/postgresql");
		assert_eq!(postgres.data_path("17"), "/var/lib/postgresql/data");
	}

	#[test]
	fn images_follow_the_template() {
		assert_eq!(
			spec(Engine::Postgres).image("18"),
			"docker.io/library/postgres:18-alpine"
		);
		assert_eq!(
			spec(Engine::Mysql).image("9.7"),
			"docker.io/library/mysql:9.7"
		);
	}

	#[test]
	fn urls_use_the_engine_scheme() {
		let c = credentials();
		assert_eq!(
			spec(Engine::Postgres).url("h", 5432, &c),
			"postgres://podkit:pw@h:5432/podkit"
		);
		assert_eq!(
			spec(Engine::Mongodb).url("h", 27017, &c),
			"mongodb://podkit:pw@h:27017/podkit?authSource=admin"
		);
		assert_eq!(spec(Engine::Redis).url("h", 6379, &c), "redis://:pw@h:6379");
	}
}
