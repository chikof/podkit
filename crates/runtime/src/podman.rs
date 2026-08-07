use async_trait::async_trait;
use bollard::Docker;
use bollard::models::{
	ContainerCreateBody, ContainerStateStatusEnum, EndpointSettings, HostConfig,
	NetworkCreateRequest, NetworkingConfig, PortBinding, RestartPolicy as BollardRestartPolicy,
	RestartPolicyNameEnum,
};
use bollard::query_parameters::{
	BuildImageOptions, CreateContainerOptions, CreateImageOptions, InspectContainerOptions,
	LogsOptions, RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use futures_util::StreamExt;
use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::{
	BuildSpec, ContainerId, ContainerSpec, ContainerState, ContainerStatus, ImageRef, LogsQuery,
	Protocol, RestartPolicy,
};
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use std::collections::HashMap;

use crate::RuntimeError;

/// `ContainerRuntime` implementation backed by a podman REST socket (podman's
/// API is Docker-Engine-API compatible, so `bollard` talks to it unmodified).
///
/// The same impl serves local and remote hosts, the only difference is which
/// socket path it was constructed against. For a remote host, callers are
/// expected to tunnel the remote `podman.sock` to a local path first (see
/// [`crate::tunnel::SshTunnel`]) and pass that local path here.
pub struct PodmanRuntime {
	docker: Docker,
}

fn to_infra<E: std::fmt::Display>(e: E) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

impl PodmanRuntime {
	/// Connects to a podman API socket at an explicit path.
	///
	/// # Errors
	/// Returns an error if the socket cannot be reached.
	pub fn connect(socket_path: &str) -> Result<Self, RuntimeError> {
		let docker = Docker::connect_with_socket(socket_path, 120, bollard::API_DEFAULT_VERSION)?;
		Ok(Self { docker })
	}

	/// Connects to the current user's default rootless podman socket
	/// (`$XDG_RUNTIME_DIR/podman/podman.sock`, falling back to
	/// `/run/user/<uid>/podman/podman.sock`).
	///
	/// # Errors
	/// Returns an error if the uid cannot be resolved or the socket is unreachable.
	pub fn connect_local_default() -> Result<Self, RuntimeError> {
		Self::connect(&local_socket_path())
	}
}

/// Resolves the current user's default rootless podman socket path
/// (`$XDG_RUNTIME_DIR/podman/podman.sock`, falling back to
/// `/run/user/<uid>/podman/podman.sock`). Does not verify the socket exists.
#[must_use]
pub fn local_socket_path() -> String {
	match std::env::var("XDG_RUNTIME_DIR").ok() {
		Some(dir) => format!("{dir}/podman/podman.sock"),
		None => format!("/run/user/{}/podman/podman.sock", rustix_getuid()),
	}
}

fn rustix_getuid() -> u32 {
	unsafe extern "C" {
		fn getuid() -> u32;
	}
	unsafe { getuid() }
}

fn port_protocol_str(protocol: Protocol) -> &'static str {
	match protocol {
		Protocol::Tcp => "tcp",
		Protocol::Udp => "udp",
	}
}

/// Bollard's `ContainerCreateBody`/`HostConfig` shape derived from a
/// [`ContainerSpec`], split out of `create_container` just to keep that
/// function under clippy's line-count limit.
struct PodmanContainerConfig {
	exposed_ports: Option<Vec<String>>,
	port_bindings: Option<HashMap<String, Option<Vec<PortBinding>>>>,
	env: Option<Vec<String>>,
	labels: Option<HashMap<String, String>>,
	binds: Option<Vec<String>>,
	networking_config: Option<NetworkingConfig>,
	nano_cpus: Option<i64>,
	restart_policy: Option<BollardRestartPolicy>,
}

fn build_container_config(spec: &ContainerSpec) -> PodmanContainerConfig {
	let exposed_ports = if spec.ports.is_empty() {
		None
	} else {
		Some(
			spec.ports
				.iter()
				.map(|p| format!("{}/{}", p.container_port, port_protocol_str(p.protocol)))
				.collect(),
		)
	};

	let port_bindings = if spec.ports.is_empty() {
		None
	} else {
		let mut map = HashMap::new();
		for p in &spec.ports {
			map.insert(
				format!("{}/{}", p.container_port, port_protocol_str(p.protocol)),
				Some(vec![PortBinding {
					host_ip: None,
					host_port: Some(p.host_port.to_string()),
				}]),
			);
		}
		Some(map)
	};

	let env = if spec.env.is_empty() {
		None
	} else {
		Some(spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect())
	};

	let labels = if spec.labels.is_empty() {
		None
	} else {
		Some(spec.labels.iter().cloned().collect::<HashMap<_, _>>())
	};

	let binds = if spec.binds.is_empty() {
		None
	} else {
		Some(
			spec.binds
				.iter()
				.map(|(host, container)| format!("{host}:{container}"))
				.collect(),
		)
	};

	let networking_config = if spec.networks.is_empty() {
		None
	} else {
		Some(NetworkingConfig {
			endpoints_config: Some(
				spec.networks
					.iter()
					.map(|n| (n.clone(), EndpointSettings::default()))
					.collect(),
			),
		})
	};

	// Precision loss beyond ~9 decimal places of a core count is
	// irrelevant; podman itself only accepts whole nano-cpus anyway.
	#[allow(clippy::cast_possible_truncation)]
	let nano_cpus = spec
		.resource_limits
		.cpu_cores
		.map(|cores| (cores * 1_000_000_000.0) as i64);

	let restart_policy = Some(match spec.restart_policy {
		RestartPolicy::Never => BollardRestartPolicy {
			name: Some(RestartPolicyNameEnum::NO),
			maximum_retry_count: None,
		},
		RestartPolicy::Always => BollardRestartPolicy {
			name: Some(RestartPolicyNameEnum::ALWAYS),
			maximum_retry_count: None,
		},
		RestartPolicy::UnlessStopped => BollardRestartPolicy {
			name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
			maximum_retry_count: None,
		},
		RestartPolicy::OnFailure { max_retries } => BollardRestartPolicy {
			name: Some(RestartPolicyNameEnum::ON_FAILURE),
			maximum_retry_count: max_retries,
		},
	});

	PodmanContainerConfig {
		exposed_ports,
		port_bindings,
		env,
		labels,
		binds,
		networking_config,
		nano_cpus,
		restart_policy,
	}
}

#[async_trait]
impl ContainerRuntime for PodmanRuntime {
	async fn ping(&self) -> DomainResult<()> {
		self.docker.ping().await.map_err(to_infra)?;
		Ok(())
	}

	async fn build_image(&self, spec: BuildSpec) -> DomainResult<ImageRef> {
		let ImageRef(tag) = spec.tag.clone();

		let mut build = self.docker.build_image(
			BuildImageOptions {
				dockerfile: spec.dockerfile_path,
				t: Some(tag.clone()),
				rm: true,
				forcerm: true,
				..Default::default()
			},
			None,
			Some(bollard::body_full(spec.context_tar.into())),
		);

		while let Some(chunk) = build.next().await {
			let info = chunk.map_err(to_infra)?;
			if let Some(err) = info.error_detail {
				return Err(DomainError::Infrastructure(
					err.message
						.unwrap_or_else(|| "image build failed".to_string()),
				));
			}
		}

		Ok(ImageRef(tag))
	}

	async fn pull_image(&self, image: &ImageRef) -> DomainResult<()> {
		let (from_image, tag) = image
			.0
			.rsplit_once(':')
			.unwrap_or((image.0.as_str(), "latest"));

		let mut pull = self.docker.create_image(
			Some(CreateImageOptions {
				from_image: Some(from_image.to_string()),
				tag: Some(tag.to_string()),
				..Default::default()
			}),
			None,
			None,
		);

		while let Some(chunk) = pull.next().await {
			chunk.map_err(to_infra)?;
		}
		Ok(())
	}

	async fn ensure_network(&self, name: &str) -> DomainResult<()> {
		match self
			.docker
			.create_network(NetworkCreateRequest {
				name: name.to_string(),
				..Default::default()
			})
			.await
		{
			Ok(_)
			| Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 409, ..
			}) => Ok(()),
			Err(e) => Err(to_infra(e)),
		}
	}

	async fn create_container(&self, spec: ContainerSpec) -> DomainResult<ContainerId> {
		let cfg = build_container_config(&spec);

		let response = self
			.docker
			.create_container(
				Some(CreateContainerOptions {
					name: Some(spec.name),
					..Default::default()
				}),
				ContainerCreateBody {
					image: Some(spec.image.0),
					cmd: spec.command,
					env: cfg.env,
					exposed_ports: cfg.exposed_ports,
					labels: cfg.labels,
					networking_config: cfg.networking_config,
					host_config: Some(HostConfig {
						port_bindings: cfg.port_bindings,
						binds: cfg.binds,
						memory: spec.resource_limits.memory_bytes,
						nano_cpus: cfg.nano_cpus,
						restart_policy: cfg.restart_policy,
						..Default::default()
					}),
					..Default::default()
				},
			)
			.await
			.map_err(to_infra)?;

		Ok(ContainerId(response.id))
	}

	async fn start_container(&self, id: &ContainerId) -> DomainResult<()> {
		self.docker
			.start_container(&id.0, None::<StartContainerOptions>)
			.await
			.map_err(to_infra)
	}

	async fn stop_container(&self, id: &ContainerId) -> DomainResult<()> {
		self.docker
			.stop_container(&id.0, None::<StopContainerOptions>)
			.await
			.map_err(to_infra)
	}

	async fn remove_container(&self, id: &ContainerId, force: bool) -> DomainResult<()> {
		self.docker
			.remove_container(
				&id.0,
				Some(RemoveContainerOptions {
					force,
					..Default::default()
				}),
			)
			.await
			.map_err(to_infra)
	}

	async fn inspect(&self, id: &ContainerId) -> DomainResult<ContainerStatus> {
		let info = self
			.docker
			.inspect_container(&id.0, None::<InspectContainerOptions>)
			.await
			.map_err(to_infra)?;

		let state = match info.state.and_then(|s| s.status) {
			Some(ContainerStateStatusEnum::CREATED) => ContainerState::Created,
			Some(ContainerStateStatusEnum::RUNNING) => ContainerState::Running,
			Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD) => {
				ContainerState::Exited
			}
			_ => ContainerState::Unknown,
		};

		Ok(ContainerStatus {
			id: id.clone(),
			state,
		})
	}

	async fn logs(&self, id: &ContainerId, query: LogsQuery) -> DomainResult<Vec<String>> {
		let mut stream = self.docker.logs(
			&id.0,
			Some(LogsOptions {
				stdout: query.stdout,
				stderr: query.stderr,
				tail: query
					.tail
					.map_or_else(|| "all".to_string(), |n| n.to_string()),
				..Default::default()
			}),
		);

		let mut lines = Vec::new();
		while let Some(chunk) = stream.next().await {
			let output = chunk.map_err(to_infra)?;
			lines.push(output.to_string());
		}

		Ok(lines)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn make_tar(dockerfile: &[u8]) -> Vec<u8> {
		let mut tar = tar::Builder::new(Vec::new());
		let mut header = tar::Header::new_gnu();
		header.set_path("Dockerfile").unwrap();
		header.set_size(dockerfile.len() as u64);
		header.set_mode(0o644);
		header.set_cksum();
		tar.append(&header, dockerfile).unwrap();
		tar.into_inner().unwrap()
	}

	/// Exercises the full lifecycle against a real podman socket: build,
	/// create, start, logs, inspect, stop, remove. Requires a reachable
	/// rootless podman.sock (`systemctl --user enable --now podman.socket`).
	/// Run explicitly: `cargo test -p runtime -- --ignored`.
	#[tokio::test]
	#[ignore = "requires a live podman.sock"]
	async fn full_lifecycle_against_live_podman() {
		let runtime = PodmanRuntime::connect_local_default().expect("connect to podman.sock");

		let dockerfile =
			b"FROM docker.io/library/alpine:3.21\nRUN echo runtime-crate-test > /marker\nCMD [\"cat\", \"/marker\"]\n";
		let tag = ImageRef("podkit-runtime-test:latest".to_string());
		let built = runtime
			.build_image(BuildSpec {
				tag: tag.clone(),
				dockerfile_path: "Dockerfile".to_string(),
				context_tar: make_tar(dockerfile),
			})
			.await
			.expect("build image");
		assert_eq!(built, tag);

		let name = "podkit-runtime-test-container";
		let _ = runtime
			.remove_container(&ContainerId(name.to_string()), true)
			.await;

		let id = runtime
			.create_container(ContainerSpec {
				name: name.to_string(),
				image: tag.clone(),
				command: None,
				env: vec![("PODKIT_TEST".to_string(), "1".to_string())],
				ports: vec![],
				networks: vec![],
				labels: vec![],
				binds: vec![],
				resource_limits: podkit_core::domain::runtime::entity::ResourceLimits::default(),
				restart_policy: RestartPolicy::Never,
			})
			.await
			.expect("create container");

		runtime.start_container(&id).await.expect("start container");

		tokio::time::sleep(std::time::Duration::from_millis(800)).await;

		let status = runtime.inspect(&id).await.expect("inspect container");
		assert_eq!(status.id, id);

		let logs = runtime
			.logs(&id, LogsQuery::default())
			.await
			.expect("fetch logs");
		assert!(
			logs.iter().any(|l| l.contains("runtime-crate-test")),
			"expected marker line in logs, got: {logs:?}"
		);

		runtime.stop_container(&id).await.expect("stop container");
		runtime
			.remove_container(&id, true)
			.await
			.expect("remove container");
	}
}
