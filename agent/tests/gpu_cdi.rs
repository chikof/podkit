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
