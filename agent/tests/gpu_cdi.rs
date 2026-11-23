//! GPU spike: which bollard `HostConfig` shape hands the NVIDIA GPU to a
//! rootless container through podman's Docker-compatible API? Needs a CDI spec
//! (`hardware.nvidia-container-toolkit.enable = true;`) and the CUDA image.
//! Run: `cargo test -p agent --test gpu_cdi -- --ignored --nocapture`.

use agent::podman::{Podman, local_socket_path};
use bollard::models::{ContainerCreateBody, DeviceMapping, DeviceRequest, HostConfig};
use bollard::query_parameters::CreateContainerOptions;
use futures_util::StreamExt;

const IMAGE: &str = "docker.io/nvidia/cuda:12.8.1-base-ubuntu24.04";
const CDI_NAME: &str = "nvidia.com/gpu=all";

fn variants() -> Vec<(&'static str, HostConfig)> {
	vec![
		(
			"devices[path_on_host=CDI name]",
			HostConfig {
				devices: Some(vec![DeviceMapping {
					path_on_host: Some(CDI_NAME.to_string()),
					..Default::default()
				}]),
				..Default::default()
			},
		),
		(
			"device_requests[driver=cdi, device_ids]",
			HostConfig {
				device_requests: Some(vec![DeviceRequest {
					driver: Some("cdi".to_string()),
					device_ids: Some(vec![CDI_NAME.to_string()]),
					..Default::default()
				}]),
				..Default::default()
			},
		),
		(
			"device_requests[no driver, device_ids]",
			HostConfig {
				device_requests: Some(vec![DeviceRequest {
					device_ids: Some(vec![CDI_NAME.to_string()]),
					..Default::default()
				}]),
				..Default::default()
			},
		),
		(
			"device_requests[docker style: count=-1, capabilities=gpu]",
			HostConfig {
				device_requests: Some(vec![DeviceRequest {
					count: Some(-1),
					capabilities: Some(vec![vec!["gpu".to_string()]]),
					..Default::default()
				}]),
				..Default::default()
			},
		),
	]
}

/// Runs `nvidia-smi -L` in a fresh container; Ok(stdout) or Err(api/exit error).
async fn try_variant(
	podman: &Podman,
	name: &str,
	host_config: HostConfig,
) -> Result<String, String> {
	podman.remove(name).await.map_err(|e| e.to_string())?;
	let docker = podman.docker();
	docker
		.create_container(
			Some(CreateContainerOptions {
				name: Some(name.to_string()),
				..Default::default()
			}),
			ContainerCreateBody {
				image: Some(IMAGE.to_string()),
				cmd: Some(vec!["nvidia-smi".to_string(), "-L".to_string()]),
				host_config: Some(host_config),
				..Default::default()
			},
		)
		.await
		.map_err(|e| format!("create: {e}"))?;
	let result = async {
		podman
			.start(name)
			.await
			.map_err(|e| format!("start: {e}"))?;
		tokio::time::sleep(std::time::Duration::from_secs(2)).await;
		let mut out = String::new();
		let mut logs = std::pin::pin!(podman.logs(name, None, false));
		while let Some(chunk) = logs.next().await {
			out.push_str(&chunk.map_err(|e| e.to_string())?.to_string());
		}
		Ok(out)
	}
	.await;
	let _ = podman.remove(name).await;
	result
}

#[tokio::test]
#[ignore = "requires podman.sock, an NVIDIA GPU and a CDI spec"]
async fn which_host_config_exposes_the_gpu() {
	let podman = Podman::connect(&local_socket_path()).expect("connect to podman.sock");
	let mut working = Vec::new();
	for (label, host_config) in variants() {
		match try_variant(&podman, "podkit-gpu-cdi-spike", host_config).await {
			Ok(out) if out.contains("GPU 0:") => {
				println!("[OK]   {label}: {}", out.trim());
				working.push(label);
			}
			Ok(out) => println!("[FAIL] {label}: ran but no GPU: {}", out.trim()),
			Err(err) => println!("[FAIL] {label}: {err}"),
		}
	}
	assert!(!working.is_empty(), "no HostConfig variant exposed the GPU");
}

/// The same proof through the agent's HTTP API: `/v1/deploy` with
/// `DeploySpec.gpus` and a CUDA base image whose command lists the GPUs.
#[tokio::test]
#[ignore = "requires podman.sock, an NVIDIA GPU and a CDI spec"]
async fn deploy_route_passes_the_gpu_through() {
	use std::sync::Arc;

	use agent::supervisor::Supervisor;
	use agent::{Ctx, routes};
	use proto::{DeploySpec, Event, GpuDevices, GpuRequest, GpuVendor, Ingress, Source, Strategy};

	let _ = rustls::crypto::ring::default_provider().install_default();
	let podman = Podman::connect(&local_socket_path()).expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap();
	let supervisor = Supervisor::start(podman.clone(), data_dir.path()).await;
	let ctx = Arc::new(Ctx {
		docker: podman.docker().clone(),
		data_dir: data_dir.path().to_path_buf(),
		podman_socket: local_socket_path(),
		supervisor,
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, routes::router(ctx)).await });

	let spec = DeploySpec {
		service: "gpu-deploy-test".to_string(),
		instance_id: 1,
		source: Source::Image {
			reference: IMAGE.to_string(),
			pull: false,
			auth: None,
		},
		env: vec![],
		// The health check wants a container that stays up.
		command: Some(vec![
			"sh".to_string(),
			"-c".to_string(),
			"nvidia-smi -L; sleep 60".to_string(),
		]),
		container_port: None,
		routing: None,
		network_aliases: vec![],
		volumes: vec![],
		published_ports: vec![],
		memory_bytes: Some(1 << 30),
		cpu_cores: None,
		strategy: Strategy::Recreate,
		ingress: Ingress {
			http_port: 80,
			https_port: 443,
			acme_email: None,
		},
		gpus: Some(GpuRequest {
			vendor: GpuVendor::Nvidia,
			devices: GpuDevices::All,
		}),
		entrypoint: None,
		shm_bytes: None,
		stop_timeout_s: None,
		networks: Vec::new(),
		options: proto::ContainerOptions::default(),
		attach: Vec::new(),
		health: None,
	};
	let body = reqwest::Client::new()
		.post(format!("http://{addr}/v1/deploy"))
		.header("content-type", "application/json")
		.body(serde_json::to_vec(&spec).unwrap())
		.send()
		.await
		.expect("post /v1/deploy")
		.text()
		.await
		.unwrap();
	let events: Vec<Event> = body
		.lines()
		.map(|l| serde_json::from_str(l).expect("ndjson event"))
		.collect();
	let terminal = events.last().expect("at least one event");

	let name = "podkit-gpu-deploy-test-1";
	let mut logs = String::new();
	if matches!(terminal, Event::Deployed { .. }) {
		let mut stream = std::pin::pin!(podman.logs(name, None, false));
		while let Some(chunk) = stream.next().await {
			logs.push_str(&chunk.unwrap().to_string());
		}
	}
	let _ = podman.remove(name).await;

	assert!(
		matches!(terminal, Event::Deployed { .. }),
		"deploy did not succeed: {terminal:?}"
	);
	println!("{logs}");
	assert!(logs.contains("GPU 0:"), "no GPU in logs: {logs}");
}
