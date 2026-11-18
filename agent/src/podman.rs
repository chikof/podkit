//! Container operations against a Podman REST socket. Podman's API is
//! Docker-Engine-API compatible, so `bollard` talks to it unmodified.

use std::collections::HashMap;
use std::path::Path;

use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::container::LogOutput;
use bollard::models::{
	ContainerCreateBody, ContainerInspectResponse, ContainerSummary, CreateImageInfo,
	DeviceMapping, EndpointSettings, EventMessage, HealthConfig, HostConfig, HostConfigLogConfig,
	ImageInspect, ImageSummary, NetworkConnectRequest, NetworkCreateRequest, NetworkingConfig,
	PortBinding, ResourcesUlimits, RestartPolicy, RestartPolicyNameEnum, VolumeCreateRequest,
};
use bollard::query_parameters::{
	BuildImageOptions, CreateContainerOptions, CreateImageOptions, EventsOptions,
	InspectContainerOptions, ListContainersOptions, ListImagesOptions, LogsOptions,
	RemoveContainerOptions, RemoveImageOptions, StartContainerOptions, StopContainerOptions,
};
use futures_util::{Stream, StreamExt};
use proto::{GpuDevices, GpuRequest, GpuVendor, RegistryAuth};

/// Per-request timeout of the Podman client. It bounds the wait for response
/// headers only; streamed bodies (builds, logs, events) run untimed.
const REQUEST_TIMEOUT_SECS: u64 = 900;

/// Failure of a Podman operation.
#[derive(Debug, thiserror::Error)]
pub enum PodmanError {
	/// The Podman API answered with an error or could not be reached.
	#[error("{0}")]
	Api(#[from] bollard::errors::Error),
	/// An image build reported an error.
	#[error("{0}")]
	Build(String),
	/// Local file access failed.
	#[error("{0}")]
	Io(#[from] std::io::Error),
}

impl PodmanError {
	/// HTTP status Podman answered with, if any.
	#[must_use]
	pub fn status(&self) -> Option<u16> {
		match self {
			Self::Api(bollard::errors::Error::DockerResponseServerError {
				status_code, ..
			}) => Some(*status_code),
			_ => None,
		}
	}

	/// Podman said the object does not exist.
	#[must_use]
	pub fn is_not_found(&self) -> bool {
		self.status() == Some(404)
	}
}

/// Everything needed to create a container. Restart policy is always `no`
/// (the supervisor owns restarts) and logs always go to a capped `k8s-file`.
#[derive(Debug, Clone, Default)]
pub struct ContainerSpec {
	/// Unique container name.
	pub name: String,
	/// Image reference or id.
	pub image: String,
	/// Overrides the image command.
	pub command: Option<Vec<String>>,
	/// Environment.
	pub env: Vec<(String, String)>,
	/// Ports published on the host.
	pub published_ports: Vec<proto::PublishedPort>,
	/// Network to join; empty for the default one.
	pub network: String,
	/// DNS aliases on `network`.
	pub aliases: Vec<String>,
	/// Container labels.
	pub labels: Vec<(String, String)>,
	/// Volume or bind mounts as `source:destination`.
	pub binds: Vec<String>,
	/// Memory limit in bytes.
	pub memory_bytes: Option<i64>,
	/// CPU limit in cores.
	pub cpu_cores: Option<f64>,
	/// GPUs to pass through.
	pub gpus: Option<GpuRequest>,
	/// Replaces the image entrypoint.
	pub entrypoint: Option<Vec<String>>,
	/// `/dev/shm` size in bytes.
	pub shm_bytes: Option<i64>,
	/// Seconds between SIGTERM and SIGKILL on stop.
	pub stop_timeout_s: Option<u32>,
	/// Networks joined after creation, with their DNS aliases.
	pub extra_networks: Vec<(String, Vec<String>)>,
	/// Settings only compose stacks use. `restart`, `isolated`, `net_aliases`
	/// and `files` are the deployer's business, not applied here.
	pub options: proto::ContainerOptions,
}

/// Device mappings for a GPU request: the CDI-name variant for NVIDIA
/// (`docs/spikes/gpu-cdi.md`), `/dev/kfd` plus `/dev/dri` for AMD.
///
/// # Errors
/// The request is empty or not expressible for the vendor.
pub fn gpu_devices(request: &GpuRequest) -> Result<Vec<DeviceMapping>, String> {
	let device = |path: String| DeviceMapping {
		path_on_host: Some(path),
		..Default::default()
	};
	let check_ids = |ids: &[String]| -> Result<(), String> {
		if ids.is_empty() {
			return Err("gpu id list is empty".to_string());
		}
		match ids.iter().find(|id| !valid_gpu_id(id)) {
			Some(bad) => Err(format!("invalid gpu id {bad:?}")),
			None => Ok(()),
		}
	};
	match (request.vendor, &request.devices) {
		(GpuVendor::Nvidia, GpuDevices::All) => Ok(vec![device("nvidia.com/gpu=all".to_string())]),
		(GpuVendor::Nvidia, GpuDevices::Count(0)) => {
			Err("gpu count must be at least 1".to_string())
		}
		(GpuVendor::Nvidia, GpuDevices::Count(n)) => Ok((0..*n)
			.map(|i| device(format!("nvidia.com/gpu={i}")))
			.collect()),
		(GpuVendor::Nvidia, GpuDevices::Ids(ids)) => {
			check_ids(ids)?;
			Ok(ids
				.iter()
				.map(|id| device(format!("nvidia.com/gpu={id}")))
				.collect())
		}
		(GpuVendor::Amd, GpuDevices::All) => Ok(vec![
			device("/dev/kfd".to_string()),
			device("/dev/dri".to_string()),
		]),
		(GpuVendor::Amd, GpuDevices::Ids(ids)) => {
			check_ids(ids)?;
			let mut devices = vec![device("/dev/kfd".to_string())];
			devices.extend(ids.iter().map(|id| device(format!("/dev/dri/{id}"))));
			Ok(devices)
		}
		(GpuVendor::Amd, GpuDevices::Count(_)) => {
			Err("AMD gpus are requested as all or by render node id".to_string())
		}
	}
}

fn valid_gpu_id(id: &str) -> bool {
	!id.is_empty()
		&& id
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Handle on one server's Podman.
#[derive(Clone)]
pub struct Podman {
	docker: Docker,
}

impl Podman {
	/// Wraps an existing connection.
	#[must_use]
	pub fn new(docker: Docker) -> Self {
		Self { docker }
	}

	/// Connects to a Podman API socket.
	///
	/// # Errors
	/// The socket does not exist or cannot be opened.
	pub fn connect(socket_path: &str) -> Result<Self, PodmanError> {
		let docker = Docker::connect_with_socket(
			socket_path,
			REQUEST_TIMEOUT_SECS,
			bollard::API_DEFAULT_VERSION,
		)?;
		Ok(Self { docker })
	}

	/// The underlying bollard client.
	#[must_use]
	pub fn docker(&self) -> &Docker {
		&self.docker
	}

	/// Podman's reported version.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn version(&self) -> Result<String, PodmanError> {
		Ok(self.docker.version().await?.version.unwrap_or_default())
	}

	/// Podman's storage root (images and volumes live below it), when it says.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn graph_root(&self) -> Result<Option<String>, PodmanError> {
		Ok(self.docker.info().await?.docker_root_dir)
	}

	/// Creates the network unless it exists.
	///
	/// # Errors
	/// Podman rejects the request.
	pub async fn ensure_network(&self, name: &str) -> Result<(), PodmanError> {
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
			Err(e) => Err(e.into()),
		}
	}

	/// Removes a network. Podman answers 409 while containers are on it and
	/// 404 when it does not exist; a missing one is fine here.
	///
	/// # Errors
	/// Podman rejects the request.
	pub async fn remove_network(&self, name: &str) -> Result<(), PodmanError> {
		match self.docker.remove_network(name).await {
			Ok(())
			| Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 404, ..
			}) => Ok(()),
			Err(e) => Err(e.into()),
		}
	}

	/// Creates the volume unless it exists.
	///
	/// # Errors
	/// Podman rejects the request.
	pub async fn ensure_volume(&self, name: &str) -> Result<(), PodmanError> {
		match self.docker.inspect_volume(name).await {
			Ok(_) => Ok(()),
			Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 404, ..
			}) => {
				self.docker
					.create_volume(VolumeCreateRequest {
						name: Some(name.to_string()),
						..Default::default()
					})
					.await?;
				Ok(())
			}
			Err(e) => Err(e.into()),
		}
	}

	/// Removes a volume. Podman answers 409 while a container uses it and 404
	/// when it does not exist; callers decide what those mean.
	///
	/// # Errors
	/// Podman rejects the request.
	pub async fn remove_volume(&self, name: &str) -> Result<(), PodmanError> {
		self.docker
			.remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
			.await?;
		Ok(())
	}

	/// Builds an image from the tar at `context_tar`, streamed from disk. Items
	/// are raw build output chunks; a build error ends the stream with `Err`.
	///
	/// # Errors
	/// The tar cannot be opened.
	pub async fn build_image(
		&self,
		tag: &str,
		dockerfile: &str,
		context_tar: &Path,
	) -> Result<impl Stream<Item = Result<String, PodmanError>> + '_, PodmanError> {
		let file = tokio::fs::File::open(context_tar).await?;
		let body = bollard::body_try_stream(tokio_util::io::ReaderStream::new(file));
		let options = BuildImageOptions {
			dockerfile: dockerfile.to_string(),
			t: Some(tag.to_string()),
			rm: true,
			forcerm: true,
			..Default::default()
		};
		Ok(self
			.docker
			.build_image(options, None, Some(body))
			.filter_map(|item| async move {
				match item {
					Ok(info) => match info.error_detail {
						Some(err) => Some(Err(PodmanError::Build(
							err.message
								.unwrap_or_else(|| "image build failed".to_string()),
						))),
						None => info.stream.map(Ok),
					},
					Err(e) => Some(Err(e.into())),
				}
			}))
	}

	/// Pulls an image, streaming Podman's progress records.
	pub fn pull_image(
		&self,
		reference: &str,
	) -> impl Stream<Item = Result<CreateImageInfo, PodmanError>> + '_ {
		self.pull_image_as(reference, None)
	}

	/// [`Self::pull_image`] with registry credentials, for private images.
	pub fn pull_image_as(
		&self,
		reference: &str,
		auth: Option<&RegistryAuth>,
	) -> impl Stream<Item = Result<CreateImageInfo, PodmanError>> + '_ {
		let (from_image, tag) = split_reference(reference);
		let credentials = auth.map(|a| DockerCredentials {
			username: Some(a.username.clone()),
			password: Some(a.password.clone()),
			serveraddress: Some(
				a.server
					.clone()
					.unwrap_or_else(|| registry_host(reference).to_string()),
			),
			..Default::default()
		});
		self.docker
			.create_image(
				Some(CreateImageOptions {
					from_image: Some(from_image),
					tag,
					..Default::default()
				}),
				None,
				credentials,
			)
			.map(|item| item.map_err(PodmanError::from))
	}

	/// Inspects an image; `None` when it does not exist.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn inspect_image(
		&self,
		reference: &str,
	) -> Result<Option<ImageInspect>, PodmanError> {
		match self.docker.inspect_image(reference).await {
			Ok(image) => Ok(Some(image)),
			Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 404, ..
			}) => Ok(None),
			Err(e) => Err(e.into()),
		}
	}

	/// Images tagged `podkit-{service}:*`, newest first.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn service_images(&self, service: &str) -> Result<Vec<ImageSummary>, PodmanError> {
		let prefix = format!("podkit-{service}:");
		let filters = HashMap::from([("reference".to_string(), vec![format!("{prefix}*")])]);
		let mut images: Vec<ImageSummary> = self
			.docker
			.list_images(Some(ListImagesOptions {
				filters: Some(filters),
				..Default::default()
			}))
			.await?
			.into_iter()
			.filter(|i| i.repo_tags.iter().any(|t| t.starts_with(&prefix)))
			.collect();
		images.sort_by_key(|i| std::cmp::Reverse(i.created));
		Ok(images)
	}

	/// Removes an image reference (tag or id); a missing image is fine.
	///
	/// # Errors
	/// Podman rejects the removal, e.g. because a container uses the image.
	pub async fn remove_image(&self, reference: &str) -> Result<(), PodmanError> {
		match self
			.docker
			.remove_image(reference, None::<RemoveImageOptions>, None)
			.await
		{
			Ok(_)
			| Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 404, ..
			}) => Ok(()),
			Err(e) => Err(e.into()),
		}
	}

	/// Creates a container without starting it and returns its id.
	///
	/// # Errors
	/// Podman rejects the spec (name conflict, missing image, ...).
	#[allow(clippy::too_many_lines, reason = "one block per container setting")]
	pub async fn create_container(&self, spec: ContainerSpec) -> Result<String, PodmanError> {
		let exposed_ports = (!spec.published_ports.is_empty()).then(|| {
			spec.published_ports
				.iter()
				.map(|p| format!("{}/{}", p.container, p.protocol.as_str()))
				.collect::<Vec<_>>()
		});
		let port_bindings = (!spec.published_ports.is_empty()).then(|| {
			spec.published_ports
				.iter()
				.map(|p| {
					(
						format!("{}/{}", p.container, p.protocol.as_str()),
						Some(vec![PortBinding {
							host_ip: Some(
								p.host_ip.clone().unwrap_or_else(|| "0.0.0.0".to_string()),
							),
							host_port: Some(p.host.to_string()),
						}]),
					)
				})
				.collect()
		});
		let networking_config = (!spec.network.is_empty()).then(|| NetworkingConfig {
			endpoints_config: Some(HashMap::from([(
				spec.network.clone(),
				EndpointSettings {
					aliases: (!spec.aliases.is_empty()).then(|| spec.aliases.clone()),
					..Default::default()
				},
			)])),
		});
		// podman only accepts whole nano-cpus, so truncation is exact enough.
		#[allow(clippy::cast_possible_truncation)]
		let nano_cpus = spec.cpu_cores.map(|cores| (cores * 1_000_000_000.0) as i64);

		let devices = spec
			.gpus
			.as_ref()
			.map(gpu_devices)
			.transpose()
			.map_err(PodmanError::Build)?;

		let o = &spec.options;
		let non_empty = |v: &[String]| (!v.is_empty()).then(|| v.to_vec());
		let response = self
			.docker
			.create_container(
				Some(CreateContainerOptions {
					name: Some(spec.name.clone()),
					..Default::default()
				}),
				ContainerCreateBody {
					image: Some(spec.image),
					cmd: spec.command,
					entrypoint: spec.entrypoint,
					stop_timeout: spec.stop_timeout_s.map(i64::from),
					user: o.user.clone(),
					working_dir: o.working_dir.clone(),
					hostname: o.hostname.clone(),
					healthcheck: o.healthcheck.as_ref().map(health_config),
					env: (!spec.env.is_empty())
						.then(|| spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
					exposed_ports,
					labels: (!spec.labels.is_empty() || !o.labels.is_empty()).then(|| {
						spec.labels
							.iter()
							.chain(&o.labels)
							.cloned()
							.collect::<HashMap<_, _>>()
					}),
					networking_config,
					host_config: Some(HostConfig {
						devices,
						shm_size: spec.shm_bytes,
						port_bindings,
						binds: (!spec.binds.is_empty()).then_some(spec.binds),
						memory: spec.memory_bytes,
						nano_cpus,
						readonly_rootfs: o.read_only.then_some(true),
						init: o.init.then_some(true),
						tmpfs: (!o.tmpfs.is_empty()).then(|| {
							o.tmpfs
								.iter()
								.map(|path| (path.clone(), String::new()))
								.collect()
						}),
						extra_hosts: non_empty(&o.extra_hosts),
						cap_add: non_empty(&o.cap_add),
						cap_drop: non_empty(&o.cap_drop),
						dns: non_empty(&o.dns),
						group_add: non_empty(&o.group_add),
						sysctls: (!o.sysctls.is_empty())
							.then(|| o.sysctls.iter().cloned().collect()),
						ulimits: (!o.ulimits.is_empty()).then(|| {
							o.ulimits
								.iter()
								.map(|u| ResourcesUlimits {
									name: Some(u.name.clone()),
									soft: Some(u.soft),
									hard: Some(u.hard),
								})
								.collect()
						}),
						restart_policy: Some(RestartPolicy {
							name: Some(RestartPolicyNameEnum::NO),
							maximum_retry_count: None,
						}),
						log_config: Some(HostConfigLogConfig {
							typ: Some("k8s-file".to_string()),
							config: Some(HashMap::from([(
								"max-size".to_string(),
								"10m".to_string(),
							)])),
						}),
						..Default::default()
					}),
					..Default::default()
				},
			)
			.await?;
		for (network, aliases) in spec.extra_networks {
			let connected = self
				.docker
				.connect_network(
					&network,
					NetworkConnectRequest {
						container: response.id.clone(),
						endpoint_config: Some(EndpointSettings {
							aliases: (!aliases.is_empty()).then_some(aliases),
							..Default::default()
						}),
					},
				)
				.await;
			if let Err(e) = connected {
				// Do not leave a half-networked container behind.
				let _ = self.remove(&spec.name).await;
				return Err(e.into());
			}
		}
		Ok(response.id)
	}

	/// Starts a container by name or id.
	///
	/// # Errors
	/// Podman rejects the request.
	pub async fn start(&self, name: &str) -> Result<(), PodmanError> {
		self.docker
			.start_container(name, None::<StartContainerOptions>)
			.await?;
		Ok(())
	}

	/// Stops a container by name or id.
	///
	/// # Errors
	/// Podman rejects the request.
	pub async fn stop(&self, name: &str) -> Result<(), PodmanError> {
		self.docker
			.stop_container(name, None::<StopContainerOptions>)
			.await?;
		Ok(())
	}

	/// Force-removes a container; a missing one is fine.
	///
	/// # Errors
	/// Podman rejects the removal.
	pub async fn remove(&self, name: &str) -> Result<(), PodmanError> {
		match self
			.docker
			.remove_container(
				name,
				Some(RemoveContainerOptions {
					v: true,
					force: true,
					link: false,
				}),
			)
			.await
		{
			Ok(())
			| Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 404, ..
			}) => Ok(()),
			Err(e) => Err(e.into()),
		}
	}

	/// Inspects a container; `None` when it does not exist.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn inspect(
		&self,
		name: &str,
	) -> Result<Option<ContainerInspectResponse>, PodmanError> {
		match self
			.docker
			.inspect_container(name, None::<InspectContainerOptions>)
			.await
		{
			Ok(info) => Ok(Some(info)),
			Err(bollard::errors::Error::DockerResponseServerError {
				status_code: 404, ..
			}) => Ok(None),
			Err(e) => Err(e.into()),
		}
	}

	/// All containers (any state) whose name starts with `prefix`.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn list(&self, prefix: &str) -> Result<Vec<ContainerSummary>, PodmanError> {
		let filters = HashMap::from([("name".to_string(), vec![prefix.to_string()])]);
		let all = self
			.docker
			.list_containers(Some(ListContainersOptions {
				all: true,
				filters: Some(filters),
				..Default::default()
			}))
			.await?;
		Ok(all
			.into_iter()
			.filter(|c| summary_name(c).is_some_and(|n| n.starts_with(prefix)))
			.collect())
	}

	/// Containers named `podkit-{service}-{instance_id}`.
	///
	/// # Errors
	/// Podman is unreachable.
	pub async fn service_containers(
		&self,
		service: &str,
	) -> Result<Vec<ContainerSummary>, PodmanError> {
		let prefix = format!("podkit-{service}-");
		Ok(self
			.list(&prefix)
			.await?
			.into_iter()
			.filter(|c| {
				summary_name(c).is_some_and(|n| {
					let rest = &n[prefix.len()..];
					!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
				})
			})
			.collect())
	}

	/// Container logs. `tail: None` returns everything.
	pub fn logs(
		&self,
		name: &str,
		tail: Option<u32>,
		follow: bool,
	) -> impl Stream<Item = Result<LogOutput, PodmanError>> + '_ {
		self.docker
			.logs(
				name,
				Some(LogsOptions {
					follow,
					stdout: true,
					stderr: true,
					tail: tail.map_or_else(|| "all".to_string(), |n| n.to_string()),
					..Default::default()
				}),
			)
			.map(|item| item.map_err(PodmanError::from))
	}

	/// Container `die`/`died` events. Podman's compat API may name the event
	/// either way, so both are subscribed.
	pub fn die_events(&self) -> impl Stream<Item = Result<EventMessage, PodmanError>> + '_ {
		let filters = HashMap::from([
			("type".to_string(), vec!["container".to_string()]),
			(
				"event".to_string(),
				vec!["die".to_string(), "died".to_string()],
			),
		]);
		self.docker
			.events(Some(EventsOptions {
				filters: Some(filters),
				..Default::default()
			}))
			.map(|item| item.map_err(PodmanError::from))
	}
}

/// Docker's `HealthConfig`: durations are nanoseconds.
fn health_config(check: &proto::Healthcheck) -> HealthConfig {
	let ns = |ms: u64| i64::try_from(ms.saturating_mul(1_000_000)).unwrap_or(i64::MAX);
	HealthConfig {
		test: Some(check.test.clone()),
		interval: check.interval_ms.map(ns),
		timeout: check.timeout_ms.map(ns),
		retries: check.retries.map(i64::from),
		start_period: check.start_period_ms.map(ns),
		start_interval: None,
	}
}

/// The health verdict in an inspect response; `None` without a health check.
#[must_use]
pub fn health_of(info: &ContainerInspectResponse) -> Option<proto::HealthStatus> {
	use bollard::models::HealthStatusEnum as Status;
	match info.state.as_ref()?.health.as_ref()?.status? {
		Status::STARTING => Some(proto::HealthStatus::Starting),
		Status::HEALTHY => Some(proto::HealthStatus::Healthy),
		Status::UNHEALTHY => Some(proto::HealthStatus::Unhealthy),
		Status::EMPTY | Status::NONE => None,
	}
}

/// The container name of a listing entry, without Docker's leading `/`.
#[must_use]
pub fn summary_name(summary: &ContainerSummary) -> Option<&str> {
	summary
		.names
		.as_ref()?
		.first()
		.map(|n| n.trim_start_matches('/'))
}

/// The registry host of an image reference: the first path segment when it
/// looks like a host (has a `.` or `:`, or is `localhost`), else Docker Hub.
fn registry_host(reference: &str) -> &str {
	match reference.split_once('/') {
		Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => {
			first
		}
		_ => "docker.io",
	}
}

/// Splits `registry:5000/img:tag` into `(image, Some(tag))`. Digest
/// references and untagged names keep the whole string; untagged names pull `latest`.
fn split_reference(reference: &str) -> (String, Option<String>) {
	if reference.contains('@') {
		return (reference.to_string(), None);
	}
	match reference.rsplit_once(':') {
		Some((image, tag)) if !tag.contains('/') => (image.to_string(), Some(tag.to_string())),
		_ => (reference.to_string(), Some("latest".to_string())),
	}
}

/// Resolves the current user's default rootless podman socket path
/// (`$XDG_RUNTIME_DIR/podman/podman.sock`, falling back to
/// `/run/user/<uid>/podman/podman.sock`). Does not verify the socket exists.
#[must_use]
pub fn local_socket_path() -> String {
	match std::env::var("XDG_RUNTIME_DIR").ok() {
		Some(dir) => format!("{dir}/podman/podman.sock"),
		None => format!("/run/user/{}/podman/podman.sock", current_uid()),
	}
}

fn current_uid() -> u32 {
	unsafe extern "C" {
		fn getuid() -> u32;
	}
	unsafe { getuid() }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn paths(request: &GpuRequest) -> Result<Vec<String>, String> {
		gpu_devices(request).map(|d| d.into_iter().map(|m| m.path_on_host.unwrap()).collect())
	}

	fn req(vendor: GpuVendor, devices: GpuDevices) -> GpuRequest {
		GpuRequest { vendor, devices }
	}

	#[test]
	fn gpu_nvidia_maps_to_cdi_names() {
		assert_eq!(
			paths(&req(GpuVendor::Nvidia, GpuDevices::All)).unwrap(),
			["nvidia.com/gpu=all"]
		);
		assert_eq!(
			paths(&req(GpuVendor::Nvidia, GpuDevices::Count(2))).unwrap(),
			["nvidia.com/gpu=0", "nvidia.com/gpu=1"]
		);
		assert_eq!(
			paths(&req(
				GpuVendor::Nvidia,
				GpuDevices::Ids(vec!["1".into(), "GPU-abc-123".into()])
			))
			.unwrap(),
			["nvidia.com/gpu=1", "nvidia.com/gpu=GPU-abc-123"]
		);
	}

	#[test]
	fn gpu_amd_maps_to_kfd_and_dri() {
		assert_eq!(
			paths(&req(GpuVendor::Amd, GpuDevices::All)).unwrap(),
			["/dev/kfd", "/dev/dri"]
		);
		assert_eq!(
			paths(&req(
				GpuVendor::Amd,
				GpuDevices::Ids(vec!["renderD128".into()])
			))
			.unwrap(),
			["/dev/kfd", "/dev/dri/renderD128"]
		);
		assert!(paths(&req(GpuVendor::Amd, GpuDevices::Count(1))).is_err());
	}

	#[test]
	fn gpu_rejects_empty_and_hostile_requests() {
		assert!(paths(&req(GpuVendor::Nvidia, GpuDevices::Count(0))).is_err());
		assert!(paths(&req(GpuVendor::Nvidia, GpuDevices::Ids(vec![]))).is_err());
		for bad in ["", "../x", "a=b", "0 /dev/mem"] {
			for vendor in [GpuVendor::Nvidia, GpuVendor::Amd] {
				assert!(
					paths(&req(vendor, GpuDevices::Ids(vec![bad.into()]))).is_err(),
					"{bad:?}"
				);
			}
		}
	}

	#[test]
	fn stack_health_checks_are_sent_in_nanoseconds() {
		let config = health_config(&proto::Healthcheck {
			test: vec!["CMD-SHELL".into(), "true".into()],
			interval_ms: Some(2_000),
			timeout_ms: Some(500),
			retries: Some(4),
			start_period_ms: None,
		});
		assert_eq!(config.test, Some(vec!["CMD-SHELL".into(), "true".into()]));
		assert_eq!(config.interval, Some(2_000_000_000));
		assert_eq!(config.timeout, Some(500_000_000));
		assert_eq!(config.retries, Some(4));
		assert_eq!(config.start_period, None);
	}

	#[test]
	fn stack_health_status_comes_from_the_inspect_state() {
		use bollard::models::{ContainerState, Health, HealthStatusEnum};
		let inspect = |health: Option<HealthStatusEnum>| ContainerInspectResponse {
			state: Some(ContainerState {
				health: health.map(|status| Health {
					status: Some(status),
					..Default::default()
				}),
				..Default::default()
			}),
			..Default::default()
		};
		assert_eq!(health_of(&inspect(None)), None, "no health check");
		assert_eq!(health_of(&inspect(Some(HealthStatusEnum::NONE))), None);
		assert_eq!(
			health_of(&inspect(Some(HealthStatusEnum::STARTING))),
			Some(proto::HealthStatus::Starting)
		);
		assert_eq!(
			health_of(&inspect(Some(HealthStatusEnum::HEALTHY))),
			Some(proto::HealthStatus::Healthy)
		);
		assert_eq!(
			health_of(&inspect(Some(HealthStatusEnum::UNHEALTHY))),
			Some(proto::HealthStatus::Unhealthy)
		);
		assert_eq!(health_of(&ContainerInspectResponse::default()), None);
	}

	#[test]
	fn registry_host_cases() {
		for (input, host) in [
			("alpine:3", "docker.io"),
			("traefik/whoami", "docker.io"),
			("ghcr.io/me/app:1", "ghcr.io"),
			("localhost:5000/app", "localhost:5000"),
			("localhost/app", "localhost"),
		] {
			assert_eq!(registry_host(input), host, "{input}");
		}
	}

	#[test]
	fn split_reference_cases() {
		let cases = [
			("alpine:3.21", ("alpine", Some("3.21"))),
			("alpine", ("alpine", Some("latest"))),
			("localhost:5000/app", ("localhost:5000/app", Some("latest"))),
			("localhost:5000/app:v1", ("localhost:5000/app", Some("v1"))),
			(
				"docker.io/library/alpine@sha256:abc",
				("docker.io/library/alpine@sha256:abc", None),
			),
		];
		for (input, (image, tag)) in cases {
			assert_eq!(
				split_reference(input),
				(image.to_string(), tag.map(str::to_string)),
				"{input}"
			);
		}
	}
}
