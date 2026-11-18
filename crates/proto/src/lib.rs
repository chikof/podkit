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

/// `GET /v1/stats` response: one live sample of the host and its containers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
	/// The host as a whole.
	pub host: HostStats,
	/// Running containers that belong to a podkit resource.
	pub containers: Vec<ContainerStats>,
}

/// Host-wide figures. Sizes are bytes; `net_*` are counters since boot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostStats {
	/// CPU busy share since the previous sample, 0 to 100.
	pub cpu_percent: Option<f64>,
	/// Memory in use (total minus available).
	pub mem_used: u64,
	/// Physical memory.
	pub mem_total: u64,
	/// The filesystems holding the Podman storage and the agent data dir.
	pub disks: Vec<DiskStats>,
	/// Bytes received on the physical interfaces since boot.
	pub net_rx: u64,
	/// Bytes sent on the physical interfaces since boot.
	pub net_tx: u64,
	/// Utilisation of the busiest NVIDIA GPU, 0 to 100. `None` without `nvidia-smi`.
	pub gpu_util: Option<f64>,
	/// VRAM in use over all NVIDIA GPUs. `None` without `nvidia-smi`.
	pub vram_used: Option<u64>,
}

/// Space on the filesystem holding `path`, in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskStats {
	/// Directory the figures are for.
	pub path: String,
	/// Bytes in use.
	pub used: u64,
	/// Bytes free to unprivileged users.
	pub free: u64,
	/// Size of the filesystem.
	pub total: u64,
}

/// One container's resource use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerStats {
	/// Container name.
	pub name: String,
	/// What the container is part of.
	pub resource: ResourceRef,
	/// Share of the whole host's CPU, 0 to 100. `None` until a second sample exists.
	pub cpu_percent: Option<f64>,
	/// Memory without page cache, in bytes.
	pub mem_used: u64,
}

/// The podkit resource a container belongs to, from its `podkit.service` label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
	/// `application`, `database`, `model` or `stack`.
	pub kind: String,
	/// The resource's id.
	pub id: i64,
	/// The compose service, for a stack.
	pub service: Option<String>,
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
	/// GPUs to hand to the container; `None` for none. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub gpus: Option<GpuRequest>,
	/// Replaces the image entrypoint. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub entrypoint: Option<Vec<String>>,
	/// Size of `/dev/shm` in bytes; `None` keeps the runtime default.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub shm_bytes: Option<i64>,
	/// Seconds between SIGTERM and SIGKILL when the container is stopped.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stop_timeout_s: Option<u32>,
	/// Extra networks to join, created when missing: the per-stack networks of
	/// compose stacks. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub networks: Vec<String>,
	/// Container settings only compose stacks use. Absent in older payloads.
	#[serde(default, skip_serializing_if = "ContainerOptions::is_default")]
	pub options: ContainerOptions,
}

/// What the agent does when a supervised container exits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Restart {
	/// Restart it with backoff (the default for everything podkit runs).
	#[default]
	Always,
	/// Leave it stopped after it exits.
	Never,
}

/// Transport of a published port.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
	/// TCP.
	#[default]
	Tcp,
	/// UDP.
	Udp,
}

impl Protocol {
	/// Serde's `skip_serializing_if` hands over a reference.
	#[allow(clippy::trivially_copy_pass_by_ref)]
	fn is_tcp(&self) -> bool {
		*self == Self::Tcp
	}

	/// `tcp` or `udp`, as Podman spells it in port keys.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Tcp => "tcp",
			Self::Udp => "udp",
		}
	}
}

/// A container health check, in Docker's `Test` form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Healthcheck {
	/// `["CMD", arg...]`, `["CMD-SHELL", "cmd"]` or `["NONE"]`.
	pub test: Vec<String>,
	/// Time between checks, in milliseconds.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub interval_ms: Option<u64>,
	/// Time a check may take, in milliseconds.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout_ms: Option<u64>,
	/// Consecutive failures that make the container unhealthy.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub retries: Option<u32>,
	/// Time after start during which failures do not count, in milliseconds.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub start_period_ms: Option<u64>,
}

/// `ulimit` of a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ulimit {
	/// Limit name, e.g. `nofile`.
	pub name: String,
	/// Soft limit.
	pub soft: i64,
	/// Hard limit.
	pub hard: i64,
}

/// A file the agent writes on the host and mounts read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMount {
	/// Absolute path in the container.
	pub path: String,
	/// File content.
	pub content: String,
}

/// Container settings beyond what apps, databases and models need. Every
/// field is optional on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerOptions {
	/// Restart behaviour.
	#[serde(default, skip_serializing_if = "is_default")]
	pub restart: Restart,
	/// Skip the shared `podkit` network and live only on `networks`.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub isolated: bool,
	/// DNS aliases on each of `networks` (`network_aliases` are for `podkit`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub net_aliases: Vec<String>,
	/// Health check run by Podman.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub healthcheck: Option<Healthcheck>,
	/// `user[:group]` to run as.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub user: Option<String>,
	/// Working directory.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub working_dir: Option<String>,
	/// Container hostname.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub hostname: Option<String>,
	/// Read-only root filesystem.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub read_only: bool,
	/// Run an init process as PID 1.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub init: bool,
	/// tmpfs mount paths.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tmpfs: Vec<String>,
	/// `host:ip` entries for `/etc/hosts`.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub extra_hosts: Vec<String>,
	/// Resource limits.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub ulimits: Vec<Ulimit>,
	/// Capabilities to add.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub cap_add: Vec<String>,
	/// Capabilities to drop.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub cap_drop: Vec<String>,
	/// Namespaced kernel parameters.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub sysctls: Vec<(String, String)>,
	/// DNS servers.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub dns: Vec<String>,
	/// Extra groups of the container user.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub group_add: Vec<String>,
	/// Extra container labels (never `traefik.*`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub labels: Vec<(String, String)>,
	/// Files written on the host and mounted read-only.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub files: Vec<FileMount>,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
	*value == T::default()
}

impl ContainerOptions {
	fn is_default(&self) -> bool {
		*self == Self::default()
	}
}

/// GPUs a container asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuRequest {
	/// Whose GPUs.
	pub vendor: GpuVendor,
	/// Which of them.
	pub devices: GpuDevices,
}

/// GPU vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
	/// NVIDIA, through CDI (`nvidia.com/gpu=...`).
	Nvidia,
	/// AMD, through `/dev/kfd` and `/dev/dri`.
	Amd,
}

/// Which GPUs of a vendor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuDevices {
	/// Every GPU.
	All,
	/// The first `n` GPUs (NVIDIA only).
	Count(u32),
	/// Specific devices: indices or UUIDs for NVIDIA, render node names
	/// (`renderD128`) for AMD.
	Ids(Vec<String>),
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
		/// Registry credentials for the pull; `None` pulls anonymously.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		auth: Option<RegistryAuth>,
	},
}

/// Credentials for pulling from a private registry.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryAuth {
	/// Registry user.
	pub username: String,
	/// Registry password or access token.
	pub password: String,
	/// Registry host the credentials belong to (`ghcr.io`); `None` lets the
	/// agent derive it from the image reference.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub server: Option<String>,
}

impl std::fmt::Debug for RegistryAuth {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RegistryAuth")
			.field("username", &self.username)
			.field("password", &"<redacted>")
			.field("server", &self.server)
			.finish()
	}
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
	/// Mount read-only. Absent in older payloads.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub read_only: bool,
}

/// A host port mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPort {
	/// Host port.
	pub host: u16,
	/// Container port.
	pub container: u16,
	/// TCP unless said otherwise. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Protocol::is_tcp")]
	pub protocol: Protocol,
	/// Host address to bind; all addresses when absent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub host_ip: Option<String>,
}

impl PublishedPort {
	/// A TCP port on every host address.
	#[must_use]
	pub fn tcp(host: u16, container: u16) -> Self {
		Self {
			host,
			container,
			protocol: Protocol::Tcp,
			host_ip: None,
		}
	}
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
	/// Bytes of a model download written so far; not terminal.
	Progress {
		/// Size of the download volume's contents.
		bytes: u64,
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

/// Podman's verdict of a container's health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
	/// The check has not passed yet.
	Starting,
	/// The check passes.
	Healthy,
	/// The check failed `retries` times in a row.
	Unhealthy,
}

/// `GET /v1/containers/{id}/health` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerHealth {
	/// Container id.
	pub id: String,
	/// Container name.
	pub name: String,
	/// Current state.
	pub state: ContainerState,
	/// Exit code once exited.
	pub exit_code: Option<i64>,
	/// `None` when the container has no health check (or it is `NONE`).
	pub health: Option<HealthStatus>,
	/// The supervisor stopped restarting it.
	pub gave_up: bool,
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

/// `POST /v1/model-downloads` body. Streams [`Event::Log`] and
/// [`Event::Progress`], then [`Event::Done`] or [`Event::Failed`].
///
/// Running it again for the same `volume` resumes: the volume keeps the files
/// and the partial downloads.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelDownloadRequest {
	/// Podman volume that receives the files; created when missing.
	pub volume: String,
	/// Hugging Face repo id, `owner/name`.
	pub repo: String,
	/// Branch, tag or commit.
	pub revision: String,
	/// `hf download --include` patterns; empty downloads the whole repo.
	pub include: Vec<String>,
	/// Hugging Face token for gated repos. Passed to the container in its
	/// environment; the agent never logs it.
	pub token: Option<String>,
}

impl std::fmt::Debug for ModelDownloadRequest {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ModelDownloadRequest")
			.field("volume", &self.volume)
			.field("repo", &self.repo)
			.field("revision", &self.revision)
			.field("include", &self.include)
			.field("token", &self.token.as_ref().map(|_| "<redacted>"))
			.finish()
	}
}

/// `POST /v1/probes` body: one HTTP request to a container's port, made from
/// a throwaway container on the `podkit` network so it works with rootless
/// Podman, where the host cannot reach container addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
	/// Container to check; the request goes to it.
	pub container_id: String,
	/// DNS name on the `podkit` network (one of the container's aliases).
	pub host: String,
	/// Port inside the container.
	pub port: u16,
	/// `GET` or `POST`.
	pub method: String,
	/// Path and query, starting with `/`.
	pub path: String,
	/// Request body, sent as JSON for `POST`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub body: Option<String>,
	/// Seconds before the request is given up on.
	pub timeout_s: u32,
}

/// `POST /v1/probes` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResponse {
	/// False when the container has exited; no request was made then.
	pub running: bool,
	/// HTTP status, or `None` when nothing answered (connection refused, timeout).
	pub status: Option<u16>,
	/// Start of the response body or the transport error.
	pub detail: String,
}

/// The sidecar gateway's container name on the `podkit` network, and the DNS
/// name containers reach it by.
pub const GATEWAY_HOST: &str = "llm.podkit.internal";
/// Port the gateway listens on inside its container.
pub const GATEWAY_PORT: u16 = 8088;

/// One model a gateway key may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayModel {
	/// Server-side model id; usage is reported against it.
	pub model_id: i64,
	/// The `model` clients send.
	pub name: String,
	/// DNS name of the engine container on the `podkit` network.
	pub host: String,
	/// Port the engine listens on.
	pub port: u16,
	/// Whether OpenAI-style streaming requests may get `stream_options.include_usage` added.
	pub stream_usage: bool,
}

/// One gateway key and the models it may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayKey {
	/// Hex SHA-256 of the key; the key itself never reaches the agent.
	pub key_hash: String,
	/// Models the key may use.
	pub models: Vec<GatewayModel>,
}

/// `PUT /v1/gateway/config` body: every key of the server and what it routes to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConfig {
	/// All keys. An empty list stops serving everyone.
	pub keys: Vec<GatewayKey>,
	/// Models that exist on the server but belong to no key above, by name.
	/// Only used to tell "not allowed" (403) from "unknown" (404).
	#[serde(default)]
	pub other_models: Vec<String>,
}

/// Token counters of one key and model during one hour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayUsage {
	/// Hex SHA-256 of the key.
	pub key_hash: String,
	/// Server-side model id.
	pub model_id: i64,
	/// Start of the hour, unix seconds.
	pub hour: i64,
	/// Requests served.
	pub requests: u64,
	/// Prompt tokens, as far as the engine reported them.
	pub input_tokens: u64,
	/// Generated tokens, as far as the engine reported them.
	pub output_tokens: u64,
}

/// Shown when Hugging Face answers a download or listing with 401 or 403.
pub const GATED_MODEL_MESSAGE: &str =
	"gated model: accept the license on huggingface.co and add an HF token";

/// Body of every non-2xx agent response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
	/// Error message.
	pub error: String,
}

#[cfg(test)]
mod tests {
	use super::*;

	const OLD_DEPLOY: &str = r#"{
		"service": "app-1", "instance_id": 3,
		"source": {"type": "image", "reference": "alpine:3", "pull": true},
		"env": [], "command": null, "container_port": null, "routing": null,
		"network_aliases": [], "volumes": [], "published_ports": [],
		"memory_bytes": null, "cpu_cores": null, "strategy": "rolling",
		"ingress": {"http_port": 80, "https_port": 443, "acme_email": null}
	}"#;

	#[test]
	fn image_source_without_auth_round_trips() {
		let spec: DeploySpec = serde_json::from_str(OLD_DEPLOY).unwrap();
		let Source::Image { auth, .. } = &spec.source else {
			panic!("expected an image source");
		};
		assert_eq!(*auth, None);
		// `None` is not written, so older agents accept new payloads.
		assert!(!serde_json::to_string(&spec).unwrap().contains("auth"));
	}

	#[test]
	fn image_source_with_auth_round_trips_and_redacts_debug() {
		let json = OLD_DEPLOY.replace(
			r#""pull": true}"#,
			r#""pull": true, "auth": {"username": "u", "password": "hunter2", "server": "ghcr.io"}}"#,
		);
		let spec: DeploySpec = serde_json::from_str(&json).unwrap();
		let want = RegistryAuth {
			username: "u".to_string(),
			password: "hunter2".to_string(),
			server: Some("ghcr.io".to_string()),
		};
		assert!(matches!(&spec.source, Source::Image { auth: Some(a), .. } if *a == want));
		let again: DeploySpec =
			serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
		assert!(matches!(again.source, Source::Image { auth: Some(a), .. } if a == want));
		assert!(!format!("{spec:?}").contains("hunter2"));
	}

	#[test]
	fn deploy_spec_without_stack_fields_still_deserializes() {
		let spec: DeploySpec = serde_json::from_str(OLD_DEPLOY).unwrap();
		assert!(spec.networks.is_empty());
		assert_eq!(spec.options, ContainerOptions::default());
		// Defaults are not written, so older agents accept apps' payloads.
		let json = serde_json::to_string(&spec).unwrap();
		assert!(
			!json.contains("networks") && !json.contains("options"),
			"{json}"
		);
	}

	#[test]
	fn stack_options_round_trip() {
		let mut spec: DeploySpec = serde_json::from_str(OLD_DEPLOY).unwrap();
		spec.networks = vec!["podkit-stack-1".into()];
		spec.options.isolated = true;
		spec.options.restart = Restart::Never;
		spec.options.healthcheck = Some(Healthcheck {
			test: vec!["CMD-SHELL".into(), "true".into()],
			interval_ms: Some(2000),
			timeout_ms: None,
			retries: Some(3),
			start_period_ms: None,
		});
		let back: DeploySpec =
			serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
		assert_eq!(back.networks, spec.networks);
		assert_eq!(back.options, spec.options);
	}

	#[test]
	fn published_port_protocol_defaults_to_tcp() {
		let port: PublishedPort =
			serde_json::from_str(r#"{"host": 80, "container": 8080}"#).unwrap();
		assert_eq!(port.protocol, Protocol::Tcp);
		let udp = PublishedPort {
			protocol: Protocol::Udp,
			..port.clone()
		};
		assert!(serde_json::to_string(&udp).unwrap().contains("udp"));
		assert!(!serde_json::to_string(&port).unwrap().contains("protocol"));
	}

	#[test]
	fn deploy_spec_without_gpus_still_deserializes() {
		let spec: DeploySpec = serde_json::from_str(OLD_DEPLOY).unwrap();
		assert_eq!(spec.gpus, None);
		// `None` is not written, so older agents accept new payloads without gpus.
		assert!(!serde_json::to_string(&spec).unwrap().contains("gpus"));
	}

	#[test]
	fn gpu_request_wire_format() {
		let with = |gpus: &str| {
			let json = OLD_DEPLOY.replacen('{', &format!("{{\"gpus\": {gpus},"), 1);
			serde_json::from_str::<DeploySpec>(&json).unwrap().gpus
		};
		let want = |vendor, devices| Some(GpuRequest { vendor, devices });
		assert_eq!(
			with(r#"{"vendor": "nvidia", "devices": "all"}"#),
			want(GpuVendor::Nvidia, GpuDevices::All)
		);
		assert_eq!(
			with(r#"{"vendor": "nvidia", "devices": {"count": 2}}"#),
			want(GpuVendor::Nvidia, GpuDevices::Count(2))
		);
		assert_eq!(
			with(r#"{"vendor": "amd", "devices": {"ids": ["renderD128"]}}"#),
			want(GpuVendor::Amd, GpuDevices::Ids(vec!["renderD128".into()]))
		);
		assert_eq!(with("null"), None);
	}

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

/// `POST /v1/gateway/chat` body: one streaming chat completion sent through
/// the gateway, from a throwaway container on the `podkit` network (the host
/// cannot reach the gateway itself on rootless Podman). The answer is an
/// NDJSON stream of [`Event::Log`] lines, each the JSON the container printed:
/// `{"data": "<one SSE data payload>"}`, then `{"end": true}`, or
/// `{"error": "<why>"}`; then a terminal event.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayChatRequest {
	/// Gateway key sent as the bearer token. Never logged.
	pub key: String,
	/// The `/v1/chat/completions` JSON body.
	pub body: String,
	/// Seconds the gateway may stay silent before the request is given up on.
	pub timeout_s: u32,
}

impl std::fmt::Debug for GatewayChatRequest {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("GatewayChatRequest")
			.field("key", &"***")
			.field("body_bytes", &self.body.len())
			.field("timeout_s", &self.timeout_s)
			.finish()
	}
}
