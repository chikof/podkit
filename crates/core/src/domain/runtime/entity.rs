/// Opaque handle to a container as assigned by the runtime (podman/docker), not
/// a podkit-owned snowflake id. These are runtime-native ids and are not stable
/// across container recreation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContainerId(pub String);

/// An image reference, e.g. `docker.io/library/alpine:3.21` or a locally built tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef(pub String);

/// Transport protocol for a published port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
	/// TCP.
	Tcp,
	/// UDP.
	Udp,
}

/// A single host-to-container port publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMapping {
	/// Port on the host.
	pub host_port: u16,
	/// Port inside the container.
	pub container_port: u16,
	/// Transport protocol.
	pub protocol: Protocol,
}

/// What the runtime should do when a container exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
	/// Never restart, regardless of exit code.
	Never,
	/// Always restart.
	Always,
	/// Restart on non-zero exit, up to `max_retries` times (`None` = unbounded).
	OnFailure {
		/// Maximum restart attempts, or `None` for unbounded.
		max_retries: Option<i64>,
	},
	/// Restart unless the container was explicitly stopped.
	UnlessStopped,
}

/// `None` on a field means unlimited (podman/docker default). Applied at
/// container-create time.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ResourceLimits {
	/// Memory limit in bytes, or `None` for unlimited.
	pub memory_bytes: Option<i64>,
	/// Fractional CPU cores, e.g. `1.5` = 1.5 cores. Converted to podman's
	/// nano-cpus (`cores * 1e9`) by the runtime impl.
	pub cpu_cores: Option<f64>,
}

/// Everything needed to create a container. Passed to `ContainerRuntime::create`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSpec {
	/// Runtime-level container name; MUST be unique on the target host.
	pub name: String,
	/// Image to run.
	pub image: ImageRef,
	/// Overrides the image's default command when set.
	pub command: Option<Vec<String>>,
	/// Environment variables as `(key, value)` pairs.
	pub env: Vec<(String, String)>,
	/// Host port publishes. Empty for app containers, since ingress reaches
	/// them over `networks` instead; non-empty for the ingress container
	/// itself (Traefik), which is the one thing published.
	pub ports: Vec<PortMapping>,
	/// Podman networks to attach to, by name. Created if missing via
	/// `ContainerRuntime::ensure_network`; this type doesn't create them.
	pub networks: Vec<String>,
	/// Container labels, e.g. Traefik's docker-provider routing labels.
	pub labels: Vec<(String, String)>,
	/// Bind mounts as `(host_path, container_path)`.
	pub binds: Vec<(String, String)>,
	/// CPU and memory limits.
	pub resource_limits: ResourceLimits,
	/// Restart behavior on exit.
	pub restart_policy: RestartPolicy,
}

/// A build context ready to hand to the runtime: a tar archive containing at
/// least `dockerfile_path`. Building the tar is the caller's responsibility
/// (e.g. from a cloned git repo); this crate only executes the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSpec {
	/// Tag to apply to the built image.
	pub tag: ImageRef,
	/// Path to the Dockerfile within the tar archive.
	pub dockerfile_path: String,
	/// The build context as a tar archive.
	pub context_tar: Vec<u8>,
}

/// Coarse lifecycle state of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
	/// Created but not yet started.
	Created,
	/// Currently running.
	Running,
	/// Ran and stopped.
	Exited,
	/// State couldn't be determined.
	Unknown,
}

/// A container's id and current lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerStatus {
	/// The container's id.
	pub id: ContainerId,
	/// Its current lifecycle state.
	pub state: ContainerState,
}

/// Selects which buffered log lines to return. Live-follow tailing is a
/// possible future extension; this fetches a bounded snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogsQuery {
	/// Include stdout lines.
	pub stdout: bool,
	/// Include stderr lines.
	pub stderr: bool,
	/// `None` returns every buffered line; `Some(n)` returns the last `n`.
	pub tail: Option<u32>,
}

impl Default for LogsQuery {
	fn default() -> Self {
		Self {
			stdout: true,
			stderr: true,
			tail: None,
		}
	}
}
