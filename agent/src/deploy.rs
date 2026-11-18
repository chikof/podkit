//! `POST /v1/deploy`: build or pull an image, start it, wait for it to stay up,
//! then retire the previous containers of the service.
//!
//! Cancellation is dropping the future (the route does that when the client
//! disconnects), so everything created along the way is undone by [`Cleanup`]'s
//! `Drop` as well as by its explicit [`Cleanup::run`] on failure.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bollard::models::ImageSummary;
use futures_util::StreamExt;
use proto::{
	Cdi, DeploySpec, Event, GpuRequest, GpuVendor, Phase, RegistryAuth, Source, Strategy, Stream,
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::Ctx;
use crate::ingress::{self, NETWORK};
use crate::podman::{self, ContainerSpec, Podman};
use crate::source;
use crate::supervisor::Supervisor;

const HEALTH_CHECK_ATTEMPTS: u32 = 3;
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Traefik's docker provider polls; give it time to pick the new container up.
const SWAP_DELAY: Duration = Duration::from_secs(3);
const KEEP_IMAGES: usize = 3;
const FAILURE_LOG_TAIL: u32 = 20;

/// Runs a deploy, sending progress on `tx` and exactly one terminal event
/// ([`Event::Deployed`] or [`Event::Failed`]) unless the client went away.
pub async fn run(ctx: Arc<Ctx>, spec: DeploySpec, tx: mpsc::Sender<Event>) {
	let mut cleanup = Cleanup::new(&ctx);
	let terminal = match Box::pin(execute(&ctx, &spec, &tx, &mut cleanup)).await {
		Ok(deployed) => deployed,
		Err(message) => {
			cleanup.run().await;
			Event::Failed { message }
		}
	};
	let _ = tx.send(terminal).await;
}

/// Removes the container this deploy created unless it was swapped in.
struct Cleanup {
	podman: Podman,
	supervisor: Supervisor,
	container: Option<String>,
}

impl Cleanup {
	fn new(ctx: &Ctx) -> Self {
		Self {
			podman: ctx.podman(),
			supervisor: ctx.supervisor.clone(),
			container: None,
		}
	}

	fn arm(&mut self, name: String) {
		self.container = Some(name);
	}

	fn disarm(&mut self) {
		self.container = None;
	}

	async fn run(&mut self) {
		if let Some(name) = self.container.take() {
			retire(&self.podman, &self.supervisor, &name, false).await;
		}
	}
}

impl Drop for Cleanup {
	fn drop(&mut self) {
		if let Some(name) = self.container.take() {
			let (podman, supervisor) = (self.podman.clone(), self.supervisor.clone());
			tokio::spawn(async move { retire(&podman, &supervisor, &name, false).await });
		}
	}
}

/// Stops supervising a container, optionally stops it gracefully, and removes it.
async fn retire(podman: &Podman, supervisor: &Supervisor, name: &str, graceful: bool) {
	supervisor.remove(name).await;
	if graceful && let Err(e) = podman.stop(name).await {
		warn!(container = %name, error = %e, "stop failed, removing anyway");
	}
	if let Err(e) = podman.remove(name).await {
		warn!(container = %name, error = %e, "remove failed");
	}
}

async fn emit(tx: &mpsc::Sender<Event>, event: Event) -> Result<(), String> {
	tx.send(event)
		.await
		.map_err(|_| "client disconnected".to_string())
}

async fn log(
	tx: &mpsc::Sender<Event>,
	stream: Stream,
	line: impl Into<String>,
) -> Result<(), String> {
	emit(
		tx,
		Event::Log {
			stream,
			line: line.into(),
		},
	)
	.await
}

async fn phase(tx: &mpsc::Sender<Event>, phase: Phase) -> Result<(), String> {
	emit(tx, Event::Phase { phase }).await
}

async fn execute(
	ctx: &Ctx,
	spec: &DeploySpec,
	tx: &mpsc::Sender<Event>,
	cleanup: &mut Cleanup,
) -> Result<Event, String> {
	validate(spec)?;
	if let Some(gpus) = &spec.gpus {
		let kfd = std::path::Path::new("/dev/kfd").exists();
		check_gpu_support(gpus, &crate::hardware::host_cdi(), kfd)?;
	}
	let podman = ctx.podman();

	let (image, commit_sha) = match &spec.source {
		Source::Git { .. } => build_from_git(ctx, &podman, spec, tx).await?,
		Source::Image {
			reference,
			pull,
			auth,
		} => {
			let image = use_image(&podman, reference, *pull, auth.as_ref(), tx).await?;
			(image, None)
		}
	};

	let service = &spec.service;
	let new_name = format!("podkit-{service}-{}", spec.instance_id);

	phase(tx, Phase::Starting).await?;
	podman
		.ensure_network(NETWORK)
		.await
		.map_err(|e| format!("network setup failed: {e}"))?;
	if spec.routing.is_some() {
		Box::pin(ingress::ensure_traefik(
			&podman,
			&spec.ingress,
			&ctx.podman_socket,
			&ctx.data_dir,
		))
		.await
		.map_err(|e| format!("ingress setup failed: {e}"))?;
		ctx.supervisor.add(ingress::TRAEFIK_CONTAINER_NAME).await;
	}
	for volume in &spec.volumes {
		podman
			.ensure_volume(&volume.name)
			.await
			.map_err(|e| format!("volume {} setup failed: {e}", volume.name))?;
	}

	let mut old = Vec::new();
	let existing = podman
		.service_containers(service)
		.await
		.map_err(|e| format!("listing containers failed: {e}"))?;
	for name in existing.iter().filter_map(crate::podman::summary_name) {
		if spec.strategy == Strategy::Recreate || name == new_name {
			retire(&podman, &ctx.supervisor, name, true).await;
		} else {
			old.push(name.to_string());
		}
	}

	let container_id = podman
		.create_container(container_spec(spec, &image, &new_name))
		.await
		.map_err(|e| format!("container create failed: {e}"))?;
	cleanup.arm(new_name.clone());
	podman
		.start(&new_name)
		.await
		.map_err(|e| format!("container start failed: {e}"))?;
	ctx.supervisor.add(&new_name).await;

	phase(tx, Phase::HealthCheck).await?;
	if let Err(message) = health_check(&podman, &new_name).await {
		report_container_tail(&podman, &new_name, tx).await;
		return Err(message);
	}

	if spec.strategy == Strategy::Rolling {
		phase(tx, Phase::Swapping).await?;
		if !old.is_empty() {
			if spec.routing.is_some() {
				tokio::time::sleep(SWAP_DELAY).await;
			}
			for name in &old {
				retire(&podman, &ctx.supervisor, name, true).await;
			}
		}
	}
	cleanup.disarm();

	if matches!(spec.source, Source::Git { .. }) {
		prune_images(&podman, service).await;
	}

	Ok(Event::Deployed {
		container_id,
		image,
		commit_sha,
	})
}

fn valid_name(s: &str, extra: &[u8]) -> bool {
	!s.is_empty()
		&& s.bytes()
			.all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

fn validate(spec: &DeploySpec) -> Result<(), String> {
	if !valid_name(&spec.service, b"-") || spec.service.bytes().any(|b| b.is_ascii_uppercase()) {
		return Err(format!("invalid service name {:?}", spec.service));
	}
	if spec.routing.is_some() && spec.container_port.is_none() {
		return Err("routing requires container_port".to_string());
	}
	for volume in &spec.volumes {
		if !valid_name(&volume.name, b"_.-") {
			return Err(format!("invalid volume name {:?}", volume.name));
		}
		if !volume.path.starts_with('/') || volume.path.contains(':') {
			return Err(format!("invalid volume path {:?}", volume.path));
		}
	}
	if let Some(gpus) = &spec.gpus {
		podman::gpu_devices(gpus)?;
	}
	Ok(())
}

const NO_GPU_SUPPORT: &str = "see docs/spikes/gpu-cdi.md";

/// Whether this server can hand `request` to a container: NVIDIA needs a CDI
/// spec of kind `nvidia.com/gpu`, AMD needs `/dev/kfd`.
fn check_gpu_support(request: &GpuRequest, cdi: &Cdi, kfd_present: bool) -> Result<(), String> {
	match request.vendor {
		GpuVendor::Nvidia if !cdi.devices.iter().any(|k| k == "nvidia.com/gpu") => {
			Err(format!("no CDI GPU spec on this server, {NO_GPU_SUPPORT}"))
		}
		GpuVendor::Amd if !kfd_present => Err(format!(
			"no /dev/kfd on this server, cannot pass AMD GPUs through, {NO_GPU_SUPPORT}"
		)),
		_ => Ok(()),
	}
}

fn container_spec(spec: &DeploySpec, image: &str, name: &str) -> ContainerSpec {
	let mut labels = vec![("podkit.service".to_string(), spec.service.clone())];
	if let (Some(routing), Some(port)) = (&spec.routing, spec.container_port) {
		labels.extend(ingress::routing_labels(
			&spec.service,
			&routing.http_hosts,
			port,
		));
		labels.extend(ingress::secure_routing_labels(
			&spec.service,
			&routing.https_hosts,
		));
	}
	ContainerSpec {
		name: name.to_string(),
		image: image.to_string(),
		command: spec.command.clone(),
		env: spec.env.clone(),
		published_ports: spec
			.published_ports
			.iter()
			.map(|p| (p.host, p.container))
			.collect(),
		network: NETWORK.to_string(),
		aliases: spec.network_aliases.clone(),
		labels,
		binds: spec
			.volumes
			.iter()
			.map(|v| {
				let mode = if v.read_only { ":ro" } else { "" };
				format!("{}:{}{mode}", v.name, v.path)
			})
			.collect(),
		memory_bytes: spec.memory_bytes,
		cpu_cores: spec.cpu_cores,
		gpus: spec.gpus.clone(),
		entrypoint: spec.entrypoint.clone(),
		shm_bytes: spec.shm_bytes,
		stop_timeout_s: spec.stop_timeout_s,
	}
}

/// Clones, builds, and returns the image tag and commit sha.
async fn build_from_git(
	ctx: &Ctx,
	podman: &Podman,
	spec: &DeploySpec,
	tx: &mpsc::Sender<Event>,
) -> Result<(String, Option<String>), String> {
	let Source::Git {
		repo_url,
		git_ref,
		auth,
		build,
	} = &spec.source
	else {
		return Err("not a git source".to_string());
	};

	let work_root = ctx.data_dir.join("work");
	let work = tempfile::Builder::new()
		.prefix("deploy-")
		.tempdir_in(&work_root)
		.map_err(|e| format!("cannot create workspace in {}: {e}", work_root.display()))?;

	phase(tx, Phase::Cloning).await?;
	log(tx, Stream::System, format!("cloning {git_ref}")).await?;
	let prepared = source::prepare(work.path(), &ctx.data_dir, repo_url, git_ref, auth, build)
		.await
		.map_err(|e| e.to_string())?;
	let short = &prepared.commit_sha[..prepared.commit_sha.len().min(12)];
	log(tx, Stream::System, format!("checked out {short}")).await?;

	phase(tx, Phase::Building).await?;
	let tag = format!("podkit-{}:{short}", spec.service);
	let build_output = podman
		.build_image(&tag, &prepared.dockerfile, &prepared.context_tar)
		.await
		.map_err(|e| format!("image build failed: {e}"))?;
	let mut build_output = std::pin::pin!(build_output);
	while let Some(chunk) = build_output.next().await {
		let chunk = chunk.map_err(|e| format!("image build failed: {e}"))?;
		for line in chunk.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
			log(tx, Stream::Stdout, line).await?;
		}
	}
	Ok((tag, Some(prepared.commit_sha.clone())))
}

/// Makes `reference` available locally and returns the reference to run and
/// record. After a pull that is the immutable `repo@sha256:...` form when
/// podman reports one, so a later rollback runs the bytes that were
/// deployed even if the tag has moved since.
async fn use_image(
	podman: &Podman,
	reference: &str,
	pull: bool,
	auth: Option<&RegistryAuth>,
	tx: &mpsc::Sender<Event>,
) -> Result<String, String> {
	if pull {
		log(tx, Stream::System, format!("pulling {reference}")).await?;
		let mut seen = HashSet::new();
		let mut progress = std::pin::pin!(podman.pull_image_as(reference, auth));
		while let Some(info) = progress.next().await {
			let info = info.map_err(|e| format!("image pull failed: {e}"))?;
			// Progress ticks repeat one status string; log each string once.
			if let Some(status) = info.status
				&& seen.insert(status.clone())
			{
				log(tx, Stream::System, status).await?;
			}
		}
		let digest = podman
			.inspect_image(reference)
			.await
			.ok()
			.flatten()
			.and_then(|i| i.repo_digests)
			.and_then(|d| d.into_iter().next());
		return Ok(digest.unwrap_or_else(|| reference.to_string()));
	}
	match podman.inspect_image(reference).await {
		Ok(Some(_)) => Ok(reference.to_string()),
		Ok(None) => Err(format!("image {reference} no longer exists on this server")),
		Err(e) => Err(format!("image inspect failed: {e}")),
	}
}

/// The container must stay running (and not be restarted behind our back by the
/// supervisor) for every check in the window.
async fn health_check(podman: &Podman, name: &str) -> Result<(), String> {
	let mut first_start = None;
	for attempt in 1..=HEALTH_CHECK_ATTEMPTS {
		tokio::time::sleep(HEALTH_CHECK_INTERVAL).await;
		let info = podman
			.inspect(name)
			.await
			.map_err(|e| format!("inspect failed on health check {attempt}: {e}"))?
			.ok_or_else(|| format!("container disappeared on health check {attempt}"))?;
		let state = info.state.unwrap_or_default();
		if !state.running.unwrap_or(false) {
			let status = state
				.status
				.map_or_else(|| "unknown".to_string(), |s| s.to_string());
			return Err(format!(
				"container state is {status} on health check {attempt} (exit code {})",
				state.exit_code.unwrap_or(-1)
			));
		}
		match &first_start {
			None => first_start = Some(state.started_at),
			Some(started) if *started != state.started_at => {
				return Err(format!(
					"container was restarted during health check {attempt}"
				));
			}
			Some(_) => {}
		}
	}
	Ok(())
}

/// Best effort: show why a container that failed its health check died.
async fn report_container_tail(podman: &Podman, name: &str, tx: &mpsc::Sender<Event>) {
	let logs = podman.logs(name, Some(FAILURE_LOG_TAIL), false);
	let mut logs = std::pin::pin!(logs);
	while let Some(Ok(output)) = logs.next().await {
		let stream = match output {
			bollard::container::LogOutput::StdErr { .. } => Stream::Stderr,
			_ => Stream::Stdout,
		};
		for line in String::from_utf8_lossy(output.as_ref()).lines() {
			if log(tx, stream, line).await.is_err() {
				return;
			}
		}
	}
}

/// Removes old images of the service, keeping the newest [`KEEP_IMAGES`] and
/// every image a container still uses. Errors are only logged.
async fn prune_images(podman: &Podman, service: &str) {
	let (images, containers) = match tokio::join!(podman.service_images(service), podman.list("")) {
		(Ok(images), Ok(containers)) => (images, containers),
		(Err(e), _) | (_, Err(e)) => {
			warn!(service, error = %e, "listing images for pruning failed");
			return;
		}
	};
	let used: HashSet<String> = containers
		.iter()
		.filter_map(|c| c.image_id.as_deref())
		.map(|id| strip_digest_prefix(id).to_string())
		.collect();
	for tag in prune_candidates(&images, &used, service) {
		if let Err(e) = podman.remove_image(&tag).await {
			warn!(image = %tag, error = %e, "image prune failed");
		}
	}
}

fn strip_digest_prefix(id: &str) -> &str {
	id.strip_prefix("sha256:").unwrap_or(id)
}

/// Tags to remove from `images` (newest first): everything past the first
/// [`KEEP_IMAGES`] whose id no container uses.
fn prune_candidates(images: &[ImageSummary], used: &HashSet<String>, service: &str) -> Vec<String> {
	let prefix = format!("podkit-{service}:");
	images
		.iter()
		.skip(KEEP_IMAGES)
		.filter(|image| !used.contains(strip_digest_prefix(&image.id)))
		.flat_map(|image| image.repo_tags.iter())
		.filter(|tag| tag.starts_with(&prefix))
		.cloned()
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use proto::{Ingress, Routing};

	fn spec() -> DeploySpec {
		DeploySpec {
			service: "app-1".to_string(),
			instance_id: 9,
			source: Source::Image {
				reference: "alpine:3".to_string(),
				pull: true,
				auth: None,
			},
			env: vec![("A".to_string(), "b".to_string())],
			command: None,
			container_port: Some(8080),
			routing: Some(Routing {
				http_hosts: vec!["x.example.com".to_string()],
				https_hosts: vec![],
			}),
			network_aliases: vec!["db".to_string()],
			volumes: vec![],
			published_ports: vec![],
			memory_bytes: None,
			cpu_cores: None,
			strategy: Strategy::Rolling,
			ingress: Ingress {
				http_port: 80,
				https_port: 443,
				acme_email: None,
			},
			gpus: None,
			entrypoint: None,
			shm_bytes: None,
			stop_timeout_s: None,
		}
	}

	fn nvidia_cdi() -> Cdi {
		Cdi {
			available: true,
			devices: vec!["nvidia.com/gpu".to_string()],
		}
	}

	#[test]
	fn gpu_support_checks() {
		let nvidia = GpuRequest {
			vendor: GpuVendor::Nvidia,
			devices: proto::GpuDevices::All,
		};
		let amd = GpuRequest {
			vendor: GpuVendor::Amd,
			devices: proto::GpuDevices::All,
		};
		assert!(check_gpu_support(&nvidia, &nvidia_cdi(), false).is_ok());
		let err = check_gpu_support(&nvidia, &Cdi::default(), true).unwrap_err();
		assert!(err.contains("no CDI GPU spec on this server"), "{err}");
		assert!(err.contains("docs/spikes/gpu-cdi.md"));
		// An AMD CDI spec does not help NVIDIA.
		let amd_cdi = Cdi {
			available: true,
			devices: vec!["amd.com/gpu".to_string()],
		};
		assert!(check_gpu_support(&nvidia, &amd_cdi, true).is_err());
		assert!(check_gpu_support(&amd, &Cdi::default(), true).is_ok());
		assert!(check_gpu_support(&amd, &nvidia_cdi(), false).is_err());
	}

	#[test]
	fn gpu_request_reaches_the_container_spec_and_is_validated() {
		let mut with_gpu = spec();
		with_gpu.gpus = Some(GpuRequest {
			vendor: GpuVendor::Nvidia,
			devices: proto::GpuDevices::Count(2),
		});
		assert!(validate(&with_gpu).is_ok());
		assert_eq!(container_spec(&with_gpu, "img", "c").gpus, with_gpu.gpus,);
		assert!(container_spec(&spec(), "img", "c").gpus.is_none());

		with_gpu.gpus = Some(GpuRequest {
			vendor: GpuVendor::Nvidia,
			devices: proto::GpuDevices::Count(0),
		});
		assert!(validate(&with_gpu).is_err());
	}

	#[test]
	fn validate_rejects_bad_specs() {
		assert!(validate(&spec()).is_ok());

		let mut bad = spec();
		bad.service = "App_1".to_string();
		assert!(validate(&bad).is_err());

		let mut bad = spec();
		bad.container_port = None;
		assert!(validate(&bad).is_err());

		let mut bad = spec();
		bad.volumes = vec![proto::VolumeMount {
			name: "data".to_string(),
			path: "/data:ro".to_string(),
			read_only: false,
		}];
		assert!(validate(&bad).is_err());
	}

	#[test]
	fn container_spec_carries_routing_and_volumes() {
		let mut s = spec();
		s.volumes = vec![proto::VolumeMount {
			name: "data".to_string(),
			path: "/var/lib/data".to_string(),
			read_only: false,
		}];
		let c = container_spec(&s, "alpine:3", "podkit-app-1-9");
		assert_eq!(c.binds, ["data:/var/lib/data"]);
		assert_eq!(c.network, "podkit");
		assert!(
			c.labels
				.contains(&("podkit.service".to_string(), "app-1".to_string()))
		);
		assert!(
			c.labels
				.iter()
				.any(|(k, _)| k == "traefik.http.routers.app-1.rule")
		);
	}

	fn image(id: &str, tag: &str) -> ImageSummary {
		ImageSummary {
			id: id.to_string(),
			repo_tags: vec![tag.to_string()],
			..Default::default()
		}
	}

	#[test]
	fn prune_keeps_newest_three_and_used() {
		let images: Vec<ImageSummary> = (1..=6)
			.map(|n| image(&format!("sha256:{n}"), &format!("podkit-app-1:{n}")))
			.collect();
		let used = HashSet::from(["5".to_string()]);
		assert_eq!(
			prune_candidates(&images, &used, "app-1"),
			["podkit-app-1:4", "podkit-app-1:6"]
		);
	}
}
