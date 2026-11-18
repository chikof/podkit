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
	Cdi, DeploySpec, Event, GpuRequest, GpuVendor, Phase, Restart, Source, Strategy, Stream,
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
		Source::Image { reference, pull } => {
			use_image(&podman, reference, *pull, tx).await?;
			(reference.clone(), None)
		}
	};

	let service = &spec.service;
	let new_name = format!("podkit-{service}-{}", spec.instance_id);

	phase(tx, Phase::Starting).await?;
	if !spec.options.isolated {
		podman
			.ensure_network(NETWORK)
			.await
			.map_err(|e| format!("network setup failed: {e}"))?;
	}
	for network in &spec.networks {
		podman
			.ensure_network(network)
			.await
			.map_err(|e| format!("network {network} setup failed: {e}"))?;
	}
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
	let file_binds = write_files(&ctx.data_dir, spec).await?;

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
		.create_container(container_spec(spec, &image, &new_name, &file_binds))
		.await
		.map_err(|e| format!("container create failed: {e}"))?;
	cleanup.arm(new_name.clone());
	podman
		.start(&new_name)
		.await
		.map_err(|e| format!("container start failed: {e}"))?;
	if spec.options.restart == Restart::Always {
		ctx.supervisor.add(&new_name).await;
	}

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

/// Networks created for stacks; the agent refuses to join or remove any other.
pub const STACK_NETWORK_PREFIX: &str = "podkit-stack-";
/// Capabilities a container may never be given.
const DANGEROUS_CAPS: [&str; 5] = ["ALL", "SYS_ADMIN", "SYS_MODULE", "SYS_PTRACE", "SYS_RAWIO"];
const MAX_FILE_BYTES: usize = 1 << 20;

fn valid_abs_path(path: &str) -> bool {
	path.starts_with('/') && !path.contains(':') && !path.contains('\0')
}

fn validate_options(spec: &DeploySpec) -> Result<(), String> {
	let o = &spec.options;
	for network in &spec.networks {
		if !network.starts_with(STACK_NETWORK_PREFIX) || !valid_name(network, b"_.-") {
			return Err(format!("invalid network name {network:?}"));
		}
	}
	if o.isolated && (spec.networks.is_empty() || spec.routing.is_some()) {
		return Err("an isolated container needs a stack network and no routing".to_string());
	}
	for alias in spec.network_aliases.iter().chain(&o.net_aliases) {
		if !valid_name(alias, b"_.-") {
			return Err(format!("invalid network alias {alias:?}"));
		}
	}
	if let Some(cap) = o.cap_add.iter().find(|c| {
		let upper = c.to_ascii_uppercase();
		DANGEROUS_CAPS.contains(&upper.strip_prefix("CAP_").unwrap_or(&upper))
	}) {
		return Err(format!("capability {cap} is not allowed"));
	}
	if let Some(bad) = o.tmpfs.iter().find(|p| !valid_abs_path(p)) {
		return Err(format!("invalid tmpfs path {bad:?}"));
	}
	if let Some(label) = o
		.labels
		.iter()
		.find(|(k, _)| k.starts_with("traefik.") || k.starts_with("podkit."))
	{
		return Err(format!("label {:?} is reserved", label.0));
	}
	for file in &o.files {
		if !valid_abs_path(&file.path) || file.path.ends_with('/') {
			return Err(format!("invalid file mount path {:?}", file.path));
		}
		if file.content.len() > MAX_FILE_BYTES {
			return Err(format!("file mount {:?} is too large", file.path));
		}
	}
	Ok(())
}

/// Writes the spec's file mounts below `<data_dir>/stack-files/<service>/` and
/// returns the read-only binds for them. A file's name carries a hash of its
/// content, so a changed file is a new bind and an unchanged one is reused.
async fn write_files(data_dir: &std::path::Path, spec: &DeploySpec) -> Result<Vec<String>, String> {
	use sha2::{Digest, Sha256};
	use std::fmt::Write as _;

	if spec.options.files.is_empty() {
		return Ok(Vec::new());
	}
	let dir = data_dir.join("stack-files").join(&spec.service);
	tokio::fs::create_dir_all(&dir)
		.await
		.map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
	let mut binds = Vec::new();
	for file in &spec.options.files {
		let digest = Sha256::digest(format!("{}\0{}", file.path, file.content).as_bytes());
		let hash = digest.iter().take(8).fold(String::new(), |mut out, b| {
			let _ = write!(out, "{b:02x}");
			out
		});
		let base = file.path.rsplit('/').next().unwrap_or("file");
		let host = dir.join(format!("{hash}-{base}"));
		tokio::fs::write(&host, &file.content)
			.await
			.map_err(|e| format!("cannot write {}: {e}", host.display()))?;
		binds.push(format!("{}:{}:ro", host.display(), file.path));
	}
	Ok(binds)
}

fn validate(spec: &DeploySpec) -> Result<(), String> {
	if !valid_name(&spec.service, b"-") || spec.service.bytes().any(|b| b.is_ascii_uppercase()) {
		return Err(format!("invalid service name {:?}", spec.service));
	}
	if spec.routing.is_some() && spec.container_port.is_none() {
		return Err("routing requires container_port".to_string());
	}
	validate_options(spec)?;
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

fn container_spec(
	spec: &DeploySpec,
	image: &str,
	name: &str,
	file_binds: &[String],
) -> ContainerSpec {
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
	// Where the container lives: the stack network alone when isolated,
	// otherwise `podkit` first and the stack networks after it.
	let (network, aliases, extra_networks) = if spec.options.isolated {
		let mut networks = spec.networks.iter();
		let first = networks.next().cloned().unwrap_or_default();
		let rest = networks
			.map(|n| (n.clone(), spec.options.net_aliases.clone()))
			.collect();
		(first, spec.options.net_aliases.clone(), rest)
	} else {
		let rest = spec
			.networks
			.iter()
			.map(|n| (n.clone(), spec.options.net_aliases.clone()))
			.collect();
		(NETWORK.to_string(), spec.network_aliases.clone(), rest)
	};
	ContainerSpec {
		name: name.to_string(),
		image: image.to_string(),
		command: spec.command.clone(),
		env: spec.env.clone(),
		published_ports: spec.published_ports.clone(),
		network,
		aliases,
		extra_networks,
		labels,
		binds: spec
			.volumes
			.iter()
			.map(|v| {
				let mode = if v.read_only { ":ro" } else { "" };
				format!("{}:{}{mode}", v.name, v.path)
			})
			.chain(file_binds.iter().cloned())
			.collect(),
		memory_bytes: spec.memory_bytes,
		cpu_cores: spec.cpu_cores,
		gpus: spec.gpus.clone(),
		entrypoint: spec.entrypoint.clone(),
		shm_bytes: spec.shm_bytes,
		stop_timeout_s: spec.stop_timeout_s,
		options: spec.options.clone(),
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

async fn use_image(
	podman: &Podman,
	reference: &str,
	pull: bool,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	if pull {
		log(tx, Stream::System, format!("pulling {reference}")).await?;
		let mut seen = HashSet::new();
		let mut progress = std::pin::pin!(podman.pull_image(reference));
		while let Some(info) = progress.next().await {
			let info = info.map_err(|e| format!("image pull failed: {e}"))?;
			// Progress ticks repeat one status string; log each string once.
			if let Some(status) = info.status
				&& seen.insert(status.clone())
			{
				log(tx, Stream::System, status).await?;
			}
		}
		return Ok(());
	}
	match podman.inspect_image(reference).await {
		Ok(Some(_)) => Ok(()),
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
			networks: Vec::new(),
			options: proto::ContainerOptions::default(),
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
		assert_eq!(
			container_spec(&with_gpu, "img", "c", &[]).gpus,
			with_gpu.gpus,
		);
		assert!(container_spec(&spec(), "img", "c", &[]).gpus.is_none());

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
		let c = container_spec(&s, "alpine:3", "podkit-app-1-9", &[]);
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

	fn stack_spec() -> DeploySpec {
		let mut spec = spec();
		spec.service = "stack-7-db".to_string();
		spec.routing = None;
		spec.container_port = None;
		spec.network_aliases = Vec::new();
		spec.networks = vec!["podkit-stack-7".to_string()];
		spec.options.isolated = true;
		spec.options.net_aliases = vec!["db".to_string()];
		spec
	}

	#[test]
	fn stack_isolated_container_lives_on_the_stack_network_only() {
		let c = container_spec(&stack_spec(), "postgres:17", "podkit-stack-7-db-1", &[]);
		assert_eq!(c.network, "podkit-stack-7");
		assert_eq!(c.aliases, ["db"]);
		assert!(c.extra_networks.is_empty());
	}

	#[test]
	fn stack_routed_container_joins_podkit_first_and_then_the_stack_network() {
		let mut spec = stack_spec();
		spec.options.isolated = false;
		spec.network_aliases = vec!["podkit-stack-7-app".to_string()];
		spec.options.net_aliases = vec!["app".to_string()];
		let c = container_spec(&spec, "app:1", "podkit-stack-7-app-1", &[]);
		assert_eq!(c.network, "podkit");
		assert_eq!(
			c.aliases,
			["podkit-stack-7-app"],
			"unique on the shared network"
		);
		assert_eq!(
			c.extra_networks,
			[("podkit-stack-7".to_string(), vec!["app".to_string()])],
			"the compose name only resolves on the stack network"
		);
	}

	#[test]
	fn stack_ports_keep_their_protocol_and_host_address() {
		let mut spec = stack_spec();
		spec.published_ports = vec![
			proto::PublishedPort::tcp(8080, 80),
			proto::PublishedPort {
				host: 5353,
				container: 53,
				protocol: proto::Protocol::Udp,
				host_ip: Some("127.0.0.1".to_string()),
			},
		];
		let c = container_spec(&spec, "x", "n", &[]);
		assert_eq!(c.published_ports, spec.published_ports);
	}

	#[test]
	fn stack_file_mounts_are_bound_read_only_after_the_volumes() {
		let mut spec = stack_spec();
		spec.volumes = vec![proto::VolumeMount {
			name: "podkit-stack-7-data".to_string(),
			path: "/data".to_string(),
			read_only: false,
		}];
		let files = vec!["/agent/stack-files/stack-7-db/ab-app.conf:/etc/app.conf:ro".to_string()];
		let c = container_spec(&spec, "x", "n", &files);
		assert_eq!(
			c.binds,
			[
				"podkit-stack-7-data:/data",
				"/agent/stack-files/stack-7-db/ab-app.conf:/etc/app.conf:ro"
			]
		);
	}

	#[test]
	fn stack_options_are_validated() {
		assert!(validate(&stack_spec()).is_ok());
		let bad = |edit: fn(&mut DeploySpec)| {
			let mut spec = stack_spec();
			edit(&mut spec);
			validate(&spec).unwrap_err()
		};
		// Only stack networks may be joined: never `podkit` or the host's own.
		assert!(bad(|s| s.networks = vec!["podkit".into()]).contains("network"));
		assert!(bad(|s| s.networks = vec!["bridge".into()]).contains("network"));
		assert!(bad(|s| s.networks = vec!["podkit-stack-7/../x".into()]).contains("network"));
		assert!(bad(|s| s.networks.clear()).contains("isolated"));
		assert!(
			bad(|s| {
				s.routing = Some(proto::Routing {
					http_hosts: vec!["a.example.com".into()],
					https_hosts: vec![],
				});
				s.container_port = Some(80);
			})
			.contains("isolated")
		);
		assert!(bad(|s| s.options.net_aliases = vec!["bad alias".into()]).contains("alias"));
		for cap in ["SYS_ADMIN", "cap_sys_module", "ALL", "CAP_SYS_PTRACE"] {
			let mut spec = stack_spec();
			spec.options.cap_add = vec![cap.to_string()];
			assert!(
				validate(&spec).unwrap_err().contains("not allowed"),
				"{cap}"
			);
		}
		let mut spec = stack_spec();
		spec.options.cap_add = vec!["NET_BIND_SERVICE".to_string()];
		assert!(validate(&spec).is_ok());
		assert!(
			bad(|s| s.options.labels = vec![("traefik.enable".into(), "true".into())])
				.contains("reserved")
		);
		assert!(bad(|s| s.options.tmpfs = vec!["tmp".into()]).contains("tmpfs"));
		assert!(
			bad(|s| {
				s.options.files = vec![proto::FileMount {
					path: "etc/app.conf".into(),
					content: String::new(),
				}];
			})
			.contains("file mount")
		);
	}

	#[tokio::test]
	async fn stack_files_are_written_under_the_data_dir_and_changes_get_a_new_name() {
		let dir = tempfile::tempdir().unwrap();
		let mut spec = stack_spec();
		spec.options.files = vec![proto::FileMount {
			path: "/etc/app/app.conf".to_string(),
			content: "listen 80;".to_string(),
		}];
		let first = write_files(dir.path(), &spec).await.unwrap();
		assert_eq!(first.len(), 1);
		let (host, rest) = first[0].split_once(':').unwrap();
		assert_eq!(rest, "/etc/app/app.conf:ro");
		assert!(
			host.starts_with(
				&dir.path()
					.join("stack-files/stack-7-db")
					.display()
					.to_string()
			)
		);
		assert_eq!(std::fs::read_to_string(host).unwrap(), "listen 80;");

		spec.options.files[0].content = "listen 8080;".to_string();
		let second = write_files(dir.path(), &spec).await.unwrap();
		assert_ne!(first, second, "a new bind for new content");
		assert_eq!(
			std::fs::read_to_string(second[0].split_once(':').unwrap().0).unwrap(),
			"listen 8080;"
		);
		assert!(
			write_files(dir.path(), &stack_spec())
				.await
				.unwrap()
				.is_empty()
		);
	}
}
