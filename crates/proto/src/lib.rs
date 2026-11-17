//! Wire types shared by the control plane and `podkit-agent`.
//!
//! Streams are `application/x-ndjson` of [`Event`], one per line, ending with
//! exactly one terminal event ([`Event::Deployed`], [`Event::DumpWritten`],
//! [`Event::Done`] or [`Event::Failed`]).

use serde::{Deserialize, Serialize};

/// `GET /v1/health` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
	/// The agent's `CARGO_PKG_VERSION`.
	pub version: String,
	/// The version Podman reports.
	pub podman_version: String,
}

/// `GET /v1/hardware` response: a snapshot of what the host offers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hardware {
	/// Processor.
	pub cpu: Cpu,
	/// RAM.
	pub memory: Memory,
	/// The Podman graph root and the agent data dir (one entry each, when they differ).
	pub disks: Vec<Disk>,
	/// Detected GPUs, NVIDIA first.
	pub gpus: Vec<Gpu>,
	/// CDI specs on the host.
	pub cdi: Cdi,
}

/// Processor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cpu {
	/// Model name as the kernel reports it.
	pub model: String,
	/// Logical CPUs.
	pub threads: u32,
}

/// RAM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
	/// Total, in MB.
	pub total_mb: u64,
	/// Available without swapping, in MB.
	pub available_mb: u64,
}

/// Space on the filesystem holding `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
	/// Directory the figures are for.
	pub path: String,
	/// Free to unprivileged users, in MB.
	pub free_mb: u64,
	/// Total, in MB.
	pub total_mb: u64,
}

/// One GPU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
	/// `nvidia` or `amd`.
	pub vendor: String,
	/// Marketing name.
	pub name: String,
	/// Total VRAM, in MB.
	pub vram_mb: u64,
	/// `None` when the source cannot tell (AMD sysfs without `mem_info_vram_used`).
	pub vram_used_mb: Option<u64>,
}

/// Container Device Interface specs found on the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cdi {
	/// True when `/etc/cdi` or `/var/run/cdi` hold at least one spec file.
	pub available: bool,
	/// The spec files' `kind`s, e.g. `nvidia.com/gpu`.
	pub devices: Vec<String>,
}

/// `POST /v1/deploy` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploySpec {
	/// `app-{application_id}` or `db-{database_id}`.
	pub service: String,
	/// Container name suffix: deployment id for apps, job id for databases.
	pub instance_id: i64,
	/// Where the image comes from.
	pub source: Source,
	/// Container environment.
	pub env: Vec<(String, String)>,
	/// Overrides the image command.
	pub command: Option<Vec<String>>,
	/// Port Traefik routes to.
	pub container_port: Option<u16>,
	/// Traefik routing; `None` keeps the container off the ingress.
	pub routing: Option<Routing>,
	/// Extra DNS names on the `podkit` network.
	pub network_aliases: Vec<String>,
	/// Named volumes to mount.
	pub volumes: Vec<VolumeMount>,
	/// Ports published on the host.
	pub published_ports: Vec<PublishedPort>,
	/// Memory limit in bytes.
	pub memory_bytes: Option<i64>,
	/// CPU limit in cores.
	pub cpu_cores: Option<f64>,
	/// How the new container replaces old ones.
	pub strategy: Strategy,
	/// Traefik settings for the server.
	pub ingress: Ingress,
}

/// Image source of a deploy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
	/// Clone and build.
	Git {
		/// `https://`, `ssh://` or scp-form url.
		repo_url: String,
		/// Branch or tag.
		git_ref: String,
		/// Credentials for the clone.
		auth: GitAuth,
		/// How to build the image.
		build: Build,
	},
	/// Use an existing image.
	Image {
		/// Image reference.
		reference: String,
		/// Pull before use; `false` requires the image to exist locally.
		pull: bool,
	},
}

/// Git credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitAuth {
	/// Public repository.
	None,
	/// SSH deploy key (PEM).
	SshKey {
		/// The private key.
		private_key: String,
	},
	/// HTTPS access token.
	HttpsToken {
		/// The token.
		token: String,
	},
}

/// Build strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Build {
	/// Build the Dockerfile at `path`, relative to the repo root.
	Dockerfile {
		/// Relative Dockerfile path.
		path: String,
	},
	/// Generate a Dockerfile with nixpacks.
	Nixpacks,
}

/// Traefik hosts for a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
	/// Hosts served on the HTTP entrypoint.
	pub http_hosts: Vec<String>,
	/// Hosts served on the HTTPS entrypoint with ACME certs.
	pub https_hosts: Vec<String>,
}

/// A named volume mount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
	/// Podman volume name.
	pub name: String,
	/// Absolute mount path in the container.
	pub path: String,
}

/// A host port mapping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PublishedPort {
	/// Host port.
	pub host: u16,
	/// Container port.
	pub container: u16,
}

/// Replacement strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
	/// Start the new container, then retire the old ones.
	Rolling,
	/// Retire the old containers first (required with volumes).
	Recreate,
}

/// Traefik settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ingress {
	/// Host port for HTTP.
	pub http_port: u16,
	/// Host port for HTTPS.
	pub https_port: u16,
	/// ACME contact; `None` disables HTTPS.
	pub acme_email: Option<String>,
}

/// One line of an agent stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
	/// Output line.
	Log {
		/// Source stream.
		stream: Stream,
		/// The line, without trailing newline.
		line: String,
	},
	/// Progress marker.
	Phase {
		/// The phase entered.
		phase: Phase,
	},
	/// Terminal: deploy succeeded.
	Deployed {
		/// New container id.
		container_id: String,
		/// Image the container runs.
		image: String,
		/// Built commit, for git sources.
		commit_sha: Option<String>,
	},
	/// Terminal: dump file written on the agent host.
	DumpWritten {
		/// Path of the gzip file.
		path: String,
		/// Compressed size in bytes.
		size: u64,
		/// Hex SHA-256 of the compressed file.
		sha256: String,
	},
	/// Terminal: success without payload.
	Done,
	/// Terminal: failure.
	Failed {
		/// Human-readable cause.
		message: String,
	},
}

impl Event {
	/// True for the events that end a stream.
	#[must_use]
	pub fn is_terminal(&self) -> bool {
		matches!(
			self,
			Self::Deployed { .. } | Self::DumpWritten { .. } | Self::Done | Self::Failed { .. }
		)
	}
}

/// Origin of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
	/// Process stdout.
	Stdout,
	/// Process stderr.
	Stderr,
	/// Generated by podkit.
	System,
}

/// Deploy phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
	/// Cloning the repository.
	Cloning,
	/// Building the image.
	Building,
	/// Creating and starting the container.
	Starting,
	/// Waiting for the container to stay up.
	HealthCheck,
	/// Retiring old containers.
	Swapping,
}

/// `GET /v1/containers` item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerInfo {
	/// Container id.
	pub id: String,
	/// Container name.
	pub name: String,
	/// Current state.
	pub state: ContainerState,
	/// Exit code once exited.
	pub exit_code: Option<i64>,
	/// The supervisor stopped restarting it.
	pub gave_up: bool,
}

/// Container state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
	/// Created, never started.
	Created,
	/// Running.
	Running,
	/// Paused.
	Paused,
	/// Restarting.
	Restarting,
	/// Exited.
	Exited,
	/// Dead.
	Dead,
	/// Anything else Podman reports.
	Unknown,
}

/// Database engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
	/// `PostgreSQL`.
	Postgres,
	/// `MySQL`.
	Mysql,
	/// `MongoDB`.
	Mongodb,
	/// Redis.
	Redis,
}

impl Engine {
	/// Shell command that writes a logical dump to stdout. `None` for Redis.
	#[must_use]
	pub fn dump_cmd(self) -> Option<&'static str> {
		match self {
			Self::Postgres => {
				Some(r#"pg_dump --clean --if-exists --no-owner -U "$POSTGRES_USER" "$POSTGRES_DB""#)
			}
			Self::Mysql => Some(
				r#"mysqldump -uroot -p"$MYSQL_ROOT_PASSWORD" --single-transaction --set-gtid-purged=OFF --routines --triggers --databases "$MYSQL_DATABASE""#,
			),
			Self::Mongodb => Some(
				r#"mongodump --archive -u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin"#,
			),
			Self::Redis => None,
		}
	}

	/// Shell command that restores a dump read from stdin. `None` for Redis.
	#[must_use]
	pub fn restore_cmd(self) -> Option<&'static str> {
		match self {
			Self::Postgres => {
				Some(r#"psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB""#)
			}
			Self::Mysql => Some(r#"mysql -uroot -p"$MYSQL_ROOT_PASSWORD""#),
			Self::Mongodb => Some(
				r#"mongorestore --archive --drop -u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin"#,
			),
			Self::Redis => None,
		}
	}

	/// Object-key extension of a dump.
	#[must_use]
	pub fn dump_ext(self) -> Option<&'static str> {
		match self {
			Self::Postgres | Self::Mysql => Some("sql.gz"),
			Self::Mongodb => Some("archive.gz"),
			Self::Redis => None,
		}
	}
}

/// `POST /v1/backups/dump` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpRequest {
	/// Names the dump file `<data_dir>/backups/{backup_id}.gz`.
	pub backup_id: i64,
	/// Container to exec in.
	pub container: String,
	/// Engine of the database.
	pub engine: Engine,
}

/// `POST /v1/backups/upload` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRequest {
	/// Dump file path returned in [`Event::DumpWritten`].
	pub path: String,
	/// Presigned PUT url.
	pub url: String,
}

/// `POST /v1/backups/restore` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreRequest {
	/// Container to exec in.
	pub container: String,
	/// Engine of the database.
	pub engine: Engine,
	/// Presigned GET url of the gzip dump.
	pub url: String,
}

/// Body of every non-2xx agent response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
	/// Error message.
	pub error: String,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn events_use_snake_case_type_tags() {
		let line = serde_json::to_string(&Event::Log {
			stream: Stream::Stderr,
			line: "x".into(),
		})
		.unwrap();
		assert_eq!(line, r#"{"type":"log","stream":"stderr","line":"x"}"#);
		let parsed: Event =
			serde_json::from_str(r#"{"type":"phase","phase":"health_check"}"#).unwrap();
		assert_eq!(
			parsed,
			Event::Phase {
				phase: Phase::HealthCheck
			}
		);
	}
}
