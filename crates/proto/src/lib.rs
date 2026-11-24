//! Wire types shared by the control plane and `podkit-agent`.
//!
//! Streams are `application/x-ndjson` of [`Event`], one per line, ending with
//! exactly one terminal event ([`Event::Deployed`], [`Event::DumpWritten`],
//! [`Event::Done`] or [`Event::Failed`]).

use serde::{Deserialize, Serialize};

pub mod cidr;

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
	/// Traefik's counters per routed service. Empty without ingress or metrics. Absent from
	/// agents older than the HTTP charts.
	#[serde(default)]
	pub http: Vec<HttpStats>,
	/// Disk used by the volumes of each resource.
	#[serde(default)]
	pub volumes: Vec<VolumeStats>,
	/// GPU memory held by the containers of each resource (NVIDIA only).
	#[serde(default)]
	pub vram: Vec<VramStats>,
}

/// Cumulative request counters of one routed service, read from Traefik's Prometheus
/// metrics. Counters only grow (until Traefik restarts), so the reader works with the
/// difference of two samples.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpStats {
	/// The resource behind the Traefik service (`app-7`, `stack-3-web`).
	pub resource: Option<ResourceRef>,
	/// Requests answered with a 2xx status.
	pub r2xx: f64,
	/// Requests answered with a 3xx status.
	pub r3xx: f64,
	/// Requests answered with a 4xx status.
	pub r4xx: f64,
	/// Requests answered with a 5xx status.
	pub r5xx: f64,
	/// Request body bytes received.
	pub bytes_in: f64,
	/// Response body bytes sent.
	pub bytes_out: f64,
	/// Cumulative latency histogram over all statuses, `le` ascending; `None` is `+Inf`.
	pub buckets: Vec<HistogramBucket>,
}

/// One cumulative bucket of a latency histogram.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistogramBucket {
	/// Upper bound in seconds; `None` is `+Inf`.
	pub le: Option<f64>,
	/// Observations at or under `le`.
	pub count: f64,
}

/// Bytes on disk used by the volumes of one resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeStats {
	/// The owner of the volumes.
	pub resource: ResourceRef,
	/// Bytes used.
	pub used: u64,
}

/// GPU memory in use by one resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VramStats {
	/// The owner of the processes.
	pub resource: ResourceRef,
	/// Bytes of VRAM.
	pub used: u64,
}

/// What a container did that the charts mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerEventKind {
	/// The main process exited with a status other than 0.
	Exit,
	/// The kernel killed the container for using too much memory.
	Oom,
	/// Podman restarted the container.
	Restart,
}

impl ContainerEventKind {
	/// The word stored and shown.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Exit => "exit",
			Self::Oom => "oom",
			Self::Restart => "restart",
		}
	}
}

/// One entry of `GET /v1/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerEvent {
	/// Unix seconds.
	pub at: i64,
	/// What happened.
	pub kind: ContainerEventKind,
	/// The resource the container belongs to.
	pub resource: ResourceRef,
	/// Container name.
	pub container: String,
	/// The exit status, for `exit`.
	pub exit_code: Option<i64>,
}

/// How serious a stored log line says it is. Lines that say nothing count as [`Self::Info`]
/// when a search filters by level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
	/// Trace and debug.
	Debug,
	/// Everything routine.
	Info,
	/// Warnings.
	Warn,
	/// Errors, fatals, panics.
	Error,
}

impl LogLevel {
	/// The word stored, sent and shown.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Debug => "debug",
			Self::Info => "info",
			Self::Warn => "warn",
			Self::Error => "error",
		}
	}

	/// The inverse of [`Self::as_str`].
	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"debug" => Some(Self::Debug),
			"info" => Some(Self::Info),
			"warn" => Some(Self::Warn),
			"error" => Some(Self::Error),
			_ => None,
		}
	}
}

/// One stored log line: a line of the agent's per-resource log files and an element of a
/// search answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
	/// Unix milliseconds, as Podman stamped the line.
	pub ts: i64,
	/// stdout or stderr.
	pub stream: Stream,
	/// The deployment (for an application) or job the container belonged to.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub dep: Option<i64>,
	/// The level the line says it has, when it says one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub level: Option<LogLevel>,
	/// The line, without its newline.
	pub line: String,
	/// The `podkit.service` of the container (`app-7`). Not written to the files (the
	/// directory says it); filled in by a search.
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub svc: String,
}

/// `POST /v1/logs/search` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSearchRequest {
	/// `podkit.service` values (`app-7`, `stack-3`: a stack stands for all its services).
	/// Empty searches everything the agent holds.
	#[serde(default)]
	pub services: Vec<String>,
	/// Unix milliseconds, inclusive.
	#[serde(default)]
	pub since: Option<i64>,
	/// Unix milliseconds, exclusive.
	#[serde(default)]
	pub until: Option<i64>,
	/// Words the line must contain (any case), or a regular expression when `regex`.
	#[serde(default)]
	pub text: Option<String>,
	/// Whether `text` is a regular expression.
	#[serde(default)]
	pub regex: bool,
	/// Levels to keep; empty keeps all.
	#[serde(default)]
	pub levels: Vec<LogLevel>,
	/// Only lines of this deployment.
	#[serde(default)]
	pub deployment: Option<i64>,
	/// Most lines to return (the agent caps it).
	pub limit: u32,
	/// Which end of the range the limit keeps: the newest lines (the default) or the oldest.
	#[serde(default = "yes")]
	pub newest: bool,
}

fn yes() -> bool {
	true
}

/// `POST /v1/logs/search` answer: at most `limit` lines, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSearchResult {
	/// The matching lines.
	pub records: Vec<LogRecord>,
	/// The agent stopped reading before the end of the range (it took too long), so older
	/// or newer lines may be missing.
	#[serde(default)]
	pub partial: bool,
	/// More lines matched than `limit`: the ones at the far end are not in `records`.
	#[serde(default)]
	pub truncated: bool,
}

/// Where the agent forwards logs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogExportKind {
	/// Loki's push API (`POST /loki/api/v1/push`, JSON).
	Loki,
	/// OTLP over HTTP with JSON encoding (`POST /v1/logs`).
	Otlp,
}

/// Forwarding of every stored line to a log backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogExport {
	/// The protocol.
	pub kind: LogExportKind,
	/// The endpoint, complete with its path.
	pub url: String,
	/// The value of the `Authorization` header, when the backend wants one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub authorization: Option<String>,
	/// Loki's tenant (`X-Scope-OrgID`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tenant: Option<String>,
	/// Added to every line as a Loki label or an OTLP resource attribute: the team and the
	/// server's name.
	#[serde(default)]
	pub labels: Vec<(String, String)>,
	/// Whether the control plane lets requests reach private network addresses: the agent
	/// applies the same outbound policy to the backend's address as the control plane does.
	/// Loopback, link-local and metadata addresses are never reachable.
	#[serde(default)]
	pub allow_private: bool,
}

/// `PUT /v1/logs/config` body: how long and how much the agent keeps, and where it forwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogConfig {
	/// Lines older than this many days are deleted.
	pub retention_days: u32,
	/// The oldest files go while the logs take more than this many bytes.
	pub max_bytes: u64,
	/// Forwarding, if any.
	#[serde(default)]
	pub export: Option<LogExport>,
}

/// `GET /v1/logs/status` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStatus {
	/// Bytes the log files take.
	pub bytes: u64,
	/// Number of log files.
	pub files: u64,
	/// Containers being followed right now.
	pub following: u64,
	/// Lines not forwarded because the backend was slow or refused them.
	pub export_dropped: u64,
	/// Unix milliseconds of the oldest stored line (by file), if any.
	#[serde(default)]
	pub oldest: Option<i64>,
}
/// `POST /v1/containers/{id}/exec` body. The agent runs the command only in a container of a
/// tailnet sidecar service (`podkit.service` starting with [`TAILSCALE_SERVICE_PREFIX`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
	/// The command and its arguments; no shell is involved.
	pub cmd: Vec<String>,
	/// Seconds the command may take; the agent's default when absent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout_s: Option<u32>,
}

/// `POST /v1/containers/{id}/exec` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
	/// The command's exit status; `-1` when Podman could not tell.
	pub exit_code: i64,
	/// What the command wrote to stdout.
	pub output: String,
}

/// Service-name prefix of the Tailscale sidecars podkit runs next to an application.
pub const TAILSCALE_SERVICE_PREFIX: &str = "ts-";

/// `POST /v1/db-stats` body: which database containers to read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStatsRequest {
	/// One entry per database.
	pub targets: Vec<DbStatsTarget>,
}

/// One database to read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStatsTarget {
	/// The database's id, echoed in the answer.
	pub id: i64,
	/// Container to exec in.
	pub container: String,
	/// Engine of the database.
	pub engine: Engine,
}

/// One database's figures. A figure the engine cannot give cheaply is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbStat {
	/// The id of the [`DbStatsTarget`].
	pub id: i64,
	/// Open client connections.
	pub connections: Option<i64>,
	/// Size of the data in bytes.
	pub size: Option<i64>,
	/// Cumulative transactions (`PostgreSQL`) or commands (the others) since the server started.
	pub operations: Option<i64>,
	/// Memory the engine reports using, in bytes.
	pub memory: Option<i64>,
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
	/// The environment and user networks the container joins, the first one
	/// being where it lives and where Traefik reaches it. Empty (older
	/// payloads) keeps the container on the shared `podkit` network.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub attach: Vec<NetworkAttach>,
	/// Gates the swap on the new container being healthy. `None` keeps the plain
	/// "stays running" check. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub health: Option<HealthGate>,
	/// Several copies of the container behind one route. `None` is a single container.
	/// Absent in older payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub replicas: Option<Replicas>,
}

/// How many copies of a service run, and how a deploy replaces them. The copies are
/// distinct containers (`podkit-{service}-{instance_id}`, then `-r2`, `-r3`, ...) with
/// identical Traefik labels, so Traefik balances over them round robin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replicas {
	/// Containers to run, 1 to 10.
	pub count: u32,
	/// Containers a rolling update may start on top of the old ones at one time.
	pub max_surge: u32,
	/// Old containers a rolling update may take away before their replacements are healthy.
	/// Only used when `max_surge` is 0.
	pub max_unavailable: u32,
	/// Pin a client to one replica with a cookie.
	pub sticky: bool,
}

/// What makes a container healthy, and how long a deploy waits for it. The agent turns the
/// check into a Podman healthcheck, so Traefik (which skips containers that are not
/// `healthy`) sends no traffic to the new container before it passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthGate {
	/// The check itself.
	pub check: HealthCheck,
	/// Time between checks, in milliseconds.
	pub interval_ms: u64,
	/// Time one check may take, in milliseconds.
	pub timeout_ms: u64,
	/// Consecutive failures after the start period that make the container unhealthy.
	pub retries: u32,
	/// Time after start during which failures do not count, in milliseconds.
	pub start_period_ms: u64,
	/// Seconds a deploy waits for the container to become healthy before it fails.
	pub gate_timeout_s: u32,
	/// Seconds the old container keeps running once the new one is healthy.
	pub drain_s: u32,
}

/// One health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthCheck {
	/// `GET path` on the container's own port, from inside the container, and the status
	/// that counts as healthy. Needs nothing from the image: the agent binary does the request.
	Http {
		/// Path and query, starting with `/`.
		path: String,
		/// The status that means healthy.
		status: u16,
		/// The port the application listens on.
		port: u16,
	},
	/// A shell command run in the container; exit 0 is healthy.
	Command {
		/// The command line, run with `sh -c`.
		command: String,
	},
}

/// A stdio MCP server run behind `podkit-agent --mcp-bridge`: the agent binary is
/// mounted into the container and replaces its entrypoint, so the server answers
/// streamable HTTP on `port` at `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpBridge {
	/// Port the bridge listens on.
	pub port: u16,
	/// Path of the endpoint.
	pub path: String,
	/// The server's command line. Empty: the image's own entrypoint and command.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub command: Vec<String>,
}

/// A network a container joins, with the DNS aliases it has there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAttach {
	/// Podman network name.
	pub name: String,
	/// DNS aliases on this network.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub aliases: Vec<String>,
	/// Created without outbound access when missing.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub internal: bool,
}

/// Name prefix of the per-environment networks podkit creates.
pub const ENV_NETWORK_PREFIX: &str = "podkit-env-";
/// Name prefix of the user networks podkit creates.
pub const USER_NETWORK_PREFIX: &str = "podkit-net-";
/// Name prefix of the per-stack networks podkit creates.
pub const STACK_NETWORK_PREFIX: &str = "podkit-stack-";
/// The shared network of system containers (Traefik, the gateway).
pub const SYSTEM_NETWORK: &str = "podkit";

/// `POST /v1/networks` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCreate {
	/// A name with one of the podkit prefixes.
	pub name: String,
	/// No outbound access.
	#[serde(default)]
	pub internal: bool,
}

/// A Podman network as `GET /v1/networks` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
	/// Network name.
	pub name: String,
	/// No outbound access.
	pub internal: bool,
	/// Network driver, e.g. `bridge`.
	pub driver: String,
	/// Names of the containers on it.
	pub containers: Vec<String>,
}

/// `POST /v1/networks/{name}/connect` body; `disconnect` ignores `aliases`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkMember {
	/// Container name.
	pub container: String,
	/// DNS aliases on the network.
	#[serde(default)]
	pub aliases: Vec<String>,
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
	/// Run the container's stdio MCP server behind the agent's own bridge.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mcp_bridge: Option<McpBridge>,
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
		/// Cache, limits and the cache budget of the build. Absent in older payloads.
		#[serde(default)]
		options: BuildOptions,
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

/// `POST /v1/registry-login` body: asks the agent to log in to a registry the
/// way a pull would, so a wrong password is found before a deploy.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryLogin {
	/// Registry user.
	pub username: String,
	/// Registry password or access token.
	pub password: String,
	/// Registry host, with the port when it is not the default (`ghcr.io`,
	/// `127.0.0.1:5000`).
	pub server: String,
}

impl std::fmt::Debug for RegistryLogin {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RegistryLogin")
			.field("username", &self.username)
			.field("password", &"<redacted>")
			.field("server", &self.server)
			.finish()
	}
}

/// `POST /v1/registry-login` response. A refused login is an answer, not an
/// error: `ok` is false and `message` says why. The password is never in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryLoginResult {
	/// Whether the registry accepted the login.
	pub ok: bool,
	/// One line for the user.
	pub message: String,
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
		/// The user name sent with it; `x-access-token` when absent. Gitea only accepts a token
		/// together with the login of the account it belongs to.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		username: Option<String>,
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

/// How one image build runs: whether it may reuse layers, what it may use, and how big the
/// layer cache may stay afterwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildOptions {
	/// Reuse the layers of earlier builds. `false` builds every step again.
	#[serde(default = "yes")]
	pub cache: bool,
	/// CPU cap of the build containers, in cores. `None`: no cap.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cpu_cores: Option<f64>,
	/// Memory cap of the build containers, in bytes. `None`: no cap.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub memory_bytes: Option<i64>,
	/// After the build the agent prunes the layer cache down to this many bytes. `None`:
	/// leave it alone.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cache_max_bytes: Option<u64>,
}

impl Default for BuildOptions {
	fn default() -> Self {
		Self {
			cache: true,
			cpu_cores: None,
			memory_bytes: None,
			cache_max_bytes: None,
		}
	}
}

/// `POST /v1/build` body: clone and build an image, and keep it on this server (a build
/// server). The stream ends with [`Event::Built`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRequest {
	/// Service the image is for; the tag is `podkit-<service>:<short sha>`.
	pub service: String,
	/// `https://`, `ssh://` or scp-form url.
	pub repo_url: String,
	/// Branch or tag.
	pub git_ref: String,
	/// Credentials for the clone.
	pub auth: GitAuth,
	/// How to build the image.
	pub build: Build,
	/// Cache and limits.
	#[serde(default)]
	pub options: BuildOptions,
}

/// `GET /v1/build-cache` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCacheInfo {
	/// Bytes the cached layers take (layers that no tag and no container uses).
	pub bytes: u64,
	/// How many layers that is.
	pub layers: u32,
}

/// `POST /v1/build-cache/prune` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCachePrune {
	/// Keep at most this many bytes; the oldest layers go first. 0 empties the cache.
	pub max_bytes: u64,
}

/// `POST /v1/build-cache/prune` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCachePruned {
	/// Layers removed.
	pub removed: u32,
	/// Bytes they took.
	pub freed_bytes: u64,
	/// What the cache takes now.
	pub remaining_bytes: u64,
}

/// `POST /v1/images/import` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageImported {
	/// Content id of the image, `sha256:…`.
	pub image_id: String,
}

/// The header `GET /v1/images/export` answers with: the content id of the image sent.
pub const IMAGE_ID_HEADER: &str = "x-podkit-image-id";

/// Traefik hosts for a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
	/// Hosts served on the HTTP entrypoint.
	pub http_hosts: Vec<String>,
	/// Hosts served on the HTTPS entrypoint with ACME certs.
	pub https_hosts: Vec<String>,
	/// Who may reach the hosts and how; `None` adds no middleware. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub access: Option<AccessRules>,
}

/// What Traefik does to requests before they reach a service: the middlewares the agent
/// writes as labels, in the order they run (IP lists, rate limit, basic auth, body size,
/// security headers).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRules {
	/// Source ranges that may connect (CIDR or single addresses); empty allows everyone.
	#[serde(default)]
	pub allow: Vec<String>,
	/// Source ranges refused. Traefik has no deny list, so the agent subtracts these from
	/// `allow` (see [`cidr::effective_allow`]).
	#[serde(default)]
	pub deny: Vec<String>,
	/// Requests per second one client may average; `None` has no limit.
	#[serde(default)]
	pub rate_average: Option<u32>,
	/// Requests above the average a client may send in a burst.
	#[serde(default)]
	pub rate_burst: Option<u32>,
	/// `user:hash` entries for HTTP basic auth (a bcrypt hash); empty asks for no login.
	#[serde(default)]
	pub basic_auth: Vec<String>,
	/// HSTS, frame-deny, nosniff and a referrer policy on every response.
	#[serde(default)]
	pub security_headers: bool,
	/// Largest request body in bytes; `None` has no limit.
	#[serde(default)]
	pub max_body_bytes: Option<u64>,
}

impl AccessRules {
	/// Whether any middleware would be written.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.allow.is_empty()
			&& self.deny.is_empty()
			&& self.rate_average.is_none()
			&& self.basic_auth.is_empty()
			&& !self.security_headers
			&& self.max_body_bytes.is_none()
	}
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
	/// Terminal: an image was built and is on this server (`POST /v1/build`).
	Built {
		/// The image reference.
		image: String,
		/// The commit that was built.
		commit_sha: String,
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
	/// Terminal: a volume was archived to a gzip file on the agent host.
	VolumeDumped {
		/// Path of the gzip file.
		path: String,
		/// Compressed size in bytes.
		size: u64,
		/// Hex SHA-256 of the compressed file.
		sha256: String,
		/// Entries (files, directories, links) in the tar stream.
		entries: u64,
		/// Bytes of the tar stream before compression.
		raw_bytes: u64,
	},
	/// Terminal: every part of a multipart upload is stored.
	PartsUploaded {
		/// The `ETag` response header of each part, in part order.
		etags: Vec<String>,
	},
	/// Terminal: success without payload.
	Done,
	/// Terminal: a scheduled task's command ran to its end; zero is success.
	Exited {
		/// The command's exit code.
		code: i64,
	},
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
			Self::Deployed { .. }
				| Self::Built { .. }
				| Self::DumpWritten { .. }
				| Self::Done | Self::Exited { .. }
				| Self::VolumeDumped { .. }
				| Self::PartsUploaded { .. }
				| Self::Failed { .. }
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
	/// Times the supervisor restarted it recently (its failure window). Absent from older
	/// agents.
	#[serde(default)]
	pub restarts: u32,
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

	/// Shell command that prints `connections|size|operations|memory` on one line, an empty
	/// field where the engine has no cheap answer. It runs inside the database container, so
	/// it reads the credentials from the container's own environment and sends none over
	/// the wire; every query only reads.
	///
	/// Sources: `PostgreSQL` `pg_stat_database`
	/// (<https://www.postgresql.org/docs/current/monitoring-stats.html>), `MySQL` status
	/// variables (<https://dev.mysql.com/doc/refman/9.0/en/server-status-variables.html>),
	/// `MongoDB` `serverStatus` and `dbStats`
	/// (<https://www.mongodb.com/docs/manual/reference/command/serverStatus/>), Redis `INFO`
	/// (<https://redis.io/docs/latest/commands/info/>).
	#[must_use]
	pub fn stats_cmd(self) -> &'static str {
		match self {
			Self::Postgres => {
				r#"PGOPTIONS='-c default_transaction_read_only=on' psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq -c "SELECT numbackends || '|' || pg_database_size(datname) || '|' || (xact_commit + xact_rollback) || '|' FROM pg_stat_database WHERE datname = current_database()""#
			}
			Self::Mysql => {
				r#"MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql -uroot -N -B -e "SET SESSION TRANSACTION READ ONLY; SELECT CONCAT_WS('|', (SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Threads_connected'), (SELECT COALESCE(SUM(data_length + index_length), 0) FROM information_schema.tables WHERE table_schema = '$MYSQL_DATABASE'), (SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Questions'), '')""#
			}
			Self::Mongodb => {
				r#"mongosh --quiet -u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin --eval 'const s = db.serverStatus(); const d = db.getSiblingDB(process.env.MONGO_INITDB_DATABASE || "admin").stats(); const ops = Object.values(s.opcounters).reduce((a, b) => a + Number(b), 0); print([s.connections.current, d.dataSize, ops, Number(s.mem.resident) * 1048576].join("|"))'"#
			}
			Self::Redis => {
				r#"REDISCLI_AUTH="$REDIS_PASSWORD" redis-cli INFO | tr -d '\r' | awk -F: '/^connected_clients:/{c=$2} /^used_memory:/{m=$2} /^total_commands_processed:/{o=$2} END{print c "||" o "|" m}'"#
			}
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

/// `POST /v1/volume-backups/dump` body. Streams [`Event::Log`], then
/// [`Event::VolumeDumped`] or [`Event::Failed`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeDumpRequest {
	/// Names the archive file `<data_dir>/backups/{backup_id}.tar.gz`.
	pub backup_id: i64,
	/// Podman volume to archive; mounted read-only into a helper container.
	pub volume: String,
}

/// `POST /v1/volume-backups/upload` body: the file is sent as the parts of an
/// S3 multipart upload the control plane created, then deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadPartsRequest {
	/// Archive path returned in [`Event::VolumeDumped`].
	pub path: String,
	/// Bytes of every part but the last, which takes the rest.
	pub part_size: u64,
	/// Presigned `UploadPart` urls, in part order.
	pub urls: Vec<String>,
}

/// `POST /v1/volume-backups/restore` body. Streams [`Event::Log`], then
/// [`Event::Done`] or [`Event::Failed`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeRestoreRequest {
	/// Podman volume to fill; created when missing.
	pub volume: String,
	/// Presigned GET url of the gzip archive.
	pub url: String,
	/// Hex SHA-256 the downloaded archive must have; checked before anything is
	/// deleted from the volume.
	pub sha256: String,
	/// Empty the volume before extracting.
	pub wipe: bool,
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

/// `POST /v1/cron/run` body. Streams [`Event::Log`] lines of the command's output, then
/// [`Event::Exited`] with its exit code, or [`Event::Failed`] when it could not be run.
///
/// Dropping the connection stops the command: an exec is killed, a container is removed.
#[derive(Clone, Serialize, Deserialize)]
pub struct CronRunRequest {
	/// Names the pid file of an exec and the container of a one-shot run.
	pub run_id: i64,
	/// What to run, and where.
	pub target: CronTarget,
}

impl std::fmt::Debug for CronRunRequest {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// The environment of a container task carries secrets.
		f.debug_struct("CronRunRequest")
			.field("run_id", &self.run_id)
			.finish_non_exhaustive()
	}
}

/// Where a scheduled command runs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CronTarget {
	/// Inside a running container, as `sh -c <command>`.
	Exec {
		/// Container name or id.
		container: String,
		/// The shell command.
		command: String,
	},
	/// In a new container of `image`, removed afterwards.
	Container {
		/// Image reference; pulled when the server does not have it.
		image: String,
		/// Registry credentials for the pull.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		auth: Option<RegistryAuth>,
		/// The shell command; replaces the image's entrypoint and command.
		command: String,
		/// Container environment.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		env: Vec<(String, String)>,
		/// Networks to join, the first being where it lives. Without aliases.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		attach: Vec<NetworkAttach>,
		/// Memory limit in bytes.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		memory_bytes: Option<i64>,
		/// CPU limit in cores.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		cpu_cores: Option<f64>,
		/// Volumes to mount, as the application has them.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		volumes: Vec<VolumeMount>,
	},
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
	/// Networks the sidecar must be on: those of the apps that use a model
	/// and of the models. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub networks: Vec<String>,
	/// Keys of application-to-MCP-server links, one server each. Absent in older payloads.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub mcp_keys: Vec<GatewayMcpKey>,
	/// Names of the MCP servers on the server that belong to no key above.
	/// Only used to tell "not allowed" (403) from "unknown" (404).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub other_mcp: Vec<String>,
}

/// One MCP server the gateway forwards `/mcp/<name>` to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayMcp {
	/// Server-side id of the MCP server's application; usage is reported against it.
	pub server_id: i64,
	/// The `<name>` in `/mcp/<name>`.
	pub name: String,
	/// DNS name of the MCP server's container on its environment network.
	pub host: String,
	/// Port it listens on.
	pub port: u16,
	/// Path of its streamable HTTP endpoint, e.g. `/mcp`.
	pub path: String,
}

/// The key of one link, and the one MCP server it opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayMcpKey {
	/// Hex SHA-256 of the key.
	pub key_hash: String,
	/// What the key reaches.
	pub mcp: GatewayMcp,
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
	/// An MCP link's counter: `model_id` is then the MCP server's application id and
	/// the token counts are zero. Absent in older payloads.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub mcp: bool,
}

/// `POST /v1/scan/resolve` body: which image a reference names right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResolveRequest {
	/// Image reference or id on the server.
	pub image: String,
}

/// `POST /v1/scan/resolve` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResolveResponse {
	/// Content id of the image (`sha256:…`): the key scans are cached under.
	pub image_id: String,
}

/// `POST /v1/scan` body. With `image` the agent exports the image, runs Syft on
/// it and Grype on the SBOM; with `sbom` alone it only runs Grype (a rescan,
/// which never reads the image again).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanRequest {
	/// Image to scan; it must exist on the server.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub image: Option<String>,
	/// `CycloneDX` JSON of an earlier scan, for a rescan.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sbom: Option<String>,
	/// CPU cap of each scanner container.
	pub cpus: f64,
	/// Memory cap of each scanner container, in MiB.
	pub memory_mb: u32,
}

/// One vulnerability found in one package, reduced from Grype's match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanFinding {
	/// `CVE-…`, `GHSA-…` or `GO-…`.
	pub vuln_id: String,
	/// Package name.
	pub package: String,
	/// Installed version.
	pub version: String,
	/// First version that fixes it, when there is one.
	#[serde(default)]
	pub fixed_version: Option<String>,
	/// `critical`, `high`, `medium`, `low` or `unknown`.
	pub severity: String,
	/// Highest CVSS base score reported.
	#[serde(default)]
	pub cvss: Option<f64>,
	/// EPSS probability (0 to 1).
	#[serde(default)]
	pub epss: Option<f64>,
	/// Listed in CISA's Known Exploited Vulnerabilities catalog.
	#[serde(default)]
	pub kev: bool,
	/// Where to read about it.
	#[serde(default)]
	pub url: Option<String>,
}

/// `POST /v1/scan` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanResult {
	/// Content id of the scanned image; empty for a rescan.
	#[serde(default)]
	pub image_id: String,
	/// `CycloneDX` JSON, set when the agent built it (not for a rescan).
	#[serde(default)]
	pub sbom: Option<String>,
	/// What Grype found.
	pub findings: Vec<ScanFinding>,
	/// When the vulnerability database Grype used was built.
	#[serde(default)]
	pub db_built: Option<String>,
	/// `syft@1.57.2 grype@0.122.1`-style versions of the tools that ran.
	#[serde(default)]
	pub tools: String,
}

/// Shown when Hugging Face answers a download or listing with 401 or 403.
pub const GATED_MODEL_MESSAGE: &str =
	"gated model: accept the license on huggingface.co and add an HF token";

// --- terminal ------------------------------------------------------------------------

/// The web terminal speaks WebSocket on both hops (browser to server, server to agent):
/// **binary** frames are the raw terminal bytes (keystrokes towards the container, output
/// towards the browser), **text** frames are JSON: [`TerminalControl`] towards the
/// container, [`TerminalNotice`] towards the browser.
///
/// Agent route: `GET /v1/containers/{id}/terminal?cols=<n>&rows=<n>`.
pub const TERMINAL_MAX_COLS: u16 = 500;
/// See [`TERMINAL_MAX_COLS`].
pub const TERMINAL_MAX_ROWS: u16 = 200;

/// A control message from the browser, as a JSON text frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalControl {
	/// The terminal changed size.
	Resize {
		/// Width in characters.
		cols: u16,
		/// Height in lines.
		rows: u16,
	},
}

/// A message from the container side, as a JSON text frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalNotice {
	/// The shell ended. `code` is its exit status when Podman reported one.
	Exit {
		/// The exit status.
		code: Option<i64>,
	},
	/// The terminal could not start or broke.
	Error {
		/// What to tell the user.
		message: String,
	},
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
	fn attach_is_absent_unless_set() {
		let mut spec: DeploySpec = serde_json::from_str(OLD_DEPLOY).unwrap();
		assert!(spec.attach.is_empty());
		assert!(!serde_json::to_string(&spec).unwrap().contains("attach"));
		spec.attach = vec![NetworkAttach {
			name: "podkit-env-1".into(),
			aliases: vec!["db".into()],
			internal: false,
		}];
		let back: DeploySpec =
			serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
		assert_eq!(back.attach, spec.attach);
	}

	#[test]
	fn health_gate_is_absent_unless_set_and_round_trips() {
		let mut spec: DeploySpec = serde_json::from_str(OLD_DEPLOY).unwrap();
		assert!(spec.health.is_none());
		assert!(!serde_json::to_string(&spec).unwrap().contains("health"));
		spec.health = Some(HealthGate {
			check: HealthCheck::Http {
				path: "/up".into(),
				status: 200,
				port: 8080,
			},
			interval_ms: 5000,
			timeout_ms: 3000,
			retries: 3,
			start_period_ms: 10_000,
			gate_timeout_s: 90,
			drain_s: 5,
		});
		let json = serde_json::to_string(&spec).unwrap();
		assert!(json.contains(r#""check":{"type":"http","path":"/up","status":200,"port":8080}"#));
		let back: DeploySpec = serde_json::from_str(&json).unwrap();
		assert_eq!(back.health, spec.health);
		let command = HealthCheck::Command {
			command: "true".into(),
		};
		assert_eq!(
			serde_json::from_str::<HealthCheck>(&serde_json::to_string(&command).unwrap()).unwrap(),
			command
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

/// `POST /v1/gateway/mcp/tools` body: `initialize` and `tools/list` against one
/// MCP server through the gateway, from a throwaway container on the `podkit`
/// network. The answer is an NDJSON stream of [`Event::Log`] lines: one JSON
/// object, `{"tools": [...]}` or `{"error": "<why>"}`; then a terminal event.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayMcpToolsRequest {
	/// Gateway key sent as the bearer token. Never logged.
	pub key: String,
	/// The `<name>` of `/mcp/<name>`.
	pub name: String,
	/// Seconds each request may take.
	pub timeout_s: u32,
}

impl std::fmt::Debug for GatewayMcpToolsRequest {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("GatewayMcpToolsRequest")
			.field("key", &"***")
			.field("name", &self.name)
			.field("timeout_s", &self.timeout_s)
			.finish()
	}
}
