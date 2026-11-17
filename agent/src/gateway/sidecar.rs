//! The gateway as a sidecar container of the agent binary.
//!
//! On rootless Podman the host cannot reach container addresses, so a gateway
//! living in the agent process could not reach the engines. Instead the agent
//! runs its own binary in a container on the `podkit` network
//! (`podkit-agent --gateway`, aliased `llm.podkit.internal`). The agent and the
//! sidecar share one directory, bind-mounted at `/gw`:
//!
//! - `config.json`, written by the agent, polled by the sidecar;
//! - `usage/*.json`, written by the sidecar every few seconds, read and
//!   deleted by the agent when the server collects usage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use proto::{GATEWAY_HOST, GATEWAY_PORT, GatewayConfig, GatewayUsage};
use tracing::{info, warn};

use super::{Gateway, router};
use crate::ingress::NETWORK;
use crate::model_download::IMAGE;
use crate::podman::{ContainerSpec, Podman};
use crate::supervisor::Supervisor;

/// Name of the sidecar container.
pub const CONTAINER: &str = "podkit-gateway";
/// Where the shared directory is mounted in the sidecar.
pub const MOUNT: &str = "/gw";
const CONFIG: &str = "config.json";
const USAGE_DIR: &str = "usage";
const EXE_LABEL: &str = "podkit.gateway.exe";
const CONFIG_POLL: Duration = Duration::from_secs(1);
const USAGE_FLUSH: Duration = Duration::from_secs(5);
const MEMORY_BYTES: i64 = 256 << 20;

/// The shared directory under the agent's data dir.
#[must_use]
pub fn dir(data_dir: &Path) -> PathBuf {
	data_dir.join("gateway")
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	let tmp = path.with_extension("tmp");
	tokio::fs::write(&tmp, bytes).await?;
	tokio::fs::rename(&tmp, path).await
}

/// Stores the routing table for the sidecar to pick up. `Ok(true)` when it
/// differs from what was stored.
///
/// # Errors
/// The file could not be written.
pub async fn write_config(dir: &Path, config: &GatewayConfig) -> std::io::Result<bool> {
	tokio::fs::create_dir_all(dir.join(USAGE_DIR)).await?;
	let path = dir.join(CONFIG);
	let bytes = serde_json::to_vec(config)?;
	if tokio::fs::read(&path).await.is_ok_and(|old| old == bytes) {
		return Ok(false);
	}
	write_atomic(&path, &bytes).await?;
	Ok(true)
}

/// Reads the stored routing table.
pub async fn read_config(dir: &Path) -> Option<GatewayConfig> {
	let bytes = tokio::fs::read(dir.join(CONFIG)).await.ok()?;
	serde_json::from_slice(&bytes).ok()
}

/// Collects what the sidecar has written since the last call and deletes it.
/// Rows of the same key, model and hour are added up.
pub async fn drain_usage(dir: &Path) -> Vec<GatewayUsage> {
	let usage_dir = dir.join(USAGE_DIR);
	let Ok(mut entries) = tokio::fs::read_dir(&usage_dir).await else {
		return Vec::new();
	};
	let mut merged: HashMap<(String, i64, i64), GatewayUsage> = HashMap::new();
	while let Ok(Some(entry)) = entries.next_entry().await {
		let path = entry.path();
		if path.extension().is_none_or(|e| e != "json") {
			continue;
		}
		let rows: Vec<GatewayUsage> = tokio::fs::read(&path)
			.await
			.ok()
			.and_then(|b| serde_json::from_slice(&b).ok())
			.unwrap_or_else(|| {
				warn!(file = %path.display(), "gateway usage file is unreadable; dropping it");
				Vec::new()
			});
		if let Err(e) = tokio::fs::remove_file(&path).await {
			warn!(file = %path.display(), error = %e, "removing a gateway usage file failed");
			continue;
		}
		for row in rows {
			let slot = merged
				.entry((row.key_hash.clone(), row.model_id, row.hour))
				.or_insert_with(|| GatewayUsage {
					requests: 0,
					input_tokens: 0,
					output_tokens: 0,
					..row.clone()
				});
			slot.requests += row.requests;
			slot.input_tokens += row.input_tokens;
			slot.output_tokens += row.output_tokens;
		}
	}
	merged.into_values().collect()
}

/// The agent binary as a path the host can bind-mount, and a label that
/// changes when the binary does.
fn current_exe() -> Result<(PathBuf, String), String> {
	let exe = std::env::current_exe().map_err(|e| format!("cannot find the agent binary: {e}"))?;
	// A replaced binary is reported as "<path> (deleted)".
	let exe = PathBuf::from(exe.to_string_lossy().trim_end_matches(" (deleted)"));
	let modified = std::fs::metadata(&exe)
		.and_then(|m| m.modified())
		.ok()
		.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
		.map_or(0, |d| d.as_secs());
	let label = format!("{}@{modified}", exe.display());
	Ok((exe, label))
}

/// The sidecar's container spec for the agent binary at `exe`.
#[must_use]
pub fn container_spec(exe: &Path, label: &str, dir: &Path) -> ContainerSpec {
	let mut binds = vec![format!("{}:{MOUNT}", dir.display())];
	if exe.starts_with("/nix/store") {
		// A dev build links against the Nix store; the production binary is static.
		binds.push("/nix/store:/nix/store:ro".to_owned());
	} else {
		binds.push(format!("{0}:{0}:ro", exe.display()));
	}
	ContainerSpec {
		name: CONTAINER.to_owned(),
		image: IMAGE.to_owned(),
		entrypoint: Some(vec![exe.display().to_string()]),
		command: Some(vec![
			"--gateway".to_owned(),
			format!("--gateway-dir={MOUNT}"),
			format!("--listen=0.0.0.0:{GATEWAY_PORT}"),
		]),
		network: NETWORK.to_owned(),
		aliases: vec![GATEWAY_HOST.to_owned()],
		labels: vec![(EXE_LABEL.to_owned(), label.to_owned())],
		binds,
		memory_bytes: Some(MEMORY_BYTES),
		stop_timeout_s: Some(10),
		..Default::default()
	}
}

async fn ensure_image(podman: &Podman) -> Result<(), String> {
	if podman
		.inspect_image(IMAGE)
		.await
		.map_err(|e| format!("image check failed: {e}"))?
		.is_some()
	{
		return Ok(());
	}
	let mut pull = std::pin::pin!(podman.pull_image(IMAGE));
	while let Some(item) = pull.next().await {
		item.map_err(|e| format!("image pull failed: {e}"))?;
	}
	Ok(())
}

/// Makes sure the sidecar runs this agent's binary. A sidecar of an older
/// binary (the agent was upgraded) is replaced; a running current one is left alone.
///
/// # Errors
/// The container could not be created or started.
pub async fn ensure_running(
	podman: &Podman,
	supervisor: &Supervisor,
	data_dir: &Path,
) -> Result<(), String> {
	let (exe, label) = current_exe()?;
	let existing = podman
		.inspect(CONTAINER)
		.await
		.map_err(|e| format!("inspecting the gateway failed: {e}"))?;
	if let Some(info) = &existing {
		let current = info
			.config
			.as_ref()
			.and_then(|c| c.labels.as_ref())
			.and_then(|l| l.get(EXE_LABEL))
			.is_some_and(|l| *l == label);
		let running = info.state.as_ref().and_then(|s| s.running) == Some(true);
		if current && running {
			supervisor.add(CONTAINER).await;
			return Ok(());
		}
		if current {
			podman
				.start(CONTAINER)
				.await
				.map_err(|e| format!("starting the gateway failed: {e}"))?;
			supervisor.add(CONTAINER).await;
			return Ok(());
		}
		supervisor.remove(CONTAINER).await;
		podman
			.remove(CONTAINER)
			.await
			.map_err(|e| format!("removing the old gateway failed: {e}"))?;
	}
	podman
		.ensure_network(NETWORK)
		.await
		.map_err(|e| format!("creating the network failed: {e}"))?;
	ensure_image(podman).await?;
	let dir = dir(data_dir);
	podman
		.create_container(container_spec(&exe, &label, &dir))
		.await
		.map_err(|e| format!("creating the gateway failed: {e}"))?;
	podman
		.start(CONTAINER)
		.await
		.map_err(|e| format!("starting the gateway failed: {e}"))?;
	supervisor.add(CONTAINER).await;
	info!("gateway sidecar started");
	Ok(())
}

/// Applies a routing table: stores it and, when it has keys, makes sure the
/// sidecar runs.
///
/// # Errors
/// Writing the file or starting the sidecar failed.
pub async fn apply(
	podman: &Podman,
	supervisor: &Supervisor,
	data_dir: &Path,
	config: &GatewayConfig,
) -> Result<(), String> {
	write_config(&dir(data_dir), config)
		.await
		.map_err(|e| format!("writing the gateway config failed: {e}"))?;
	let exists = podman
		.inspect(CONTAINER)
		.await
		.map_err(|e| format!("inspecting the gateway failed: {e}"))?
		.is_some();
	if !config.keys.is_empty() || exists {
		ensure_running(podman, supervisor, data_dir).await?;
	}
	Ok(())
}

/// Runs the gateway server until SIGTERM or Ctrl-C: `podkit-agent --gateway`.
///
/// # Errors
/// The listen address could not be bound.
pub async fn serve(dir: PathBuf, listen: &str) -> Result<(), Box<dyn std::error::Error>> {
	let gateway = Gateway::new()?;
	let usage_dir = dir.join(USAGE_DIR);
	tokio::fs::create_dir_all(&usage_dir).await?;
	let listener = tokio::net::TcpListener::bind(listen).await?;
	info!(%listen, "gateway listening");

	let watcher = tokio::spawn(watch_config(gateway.clone(), dir.join(CONFIG)));
	let flusher = tokio::spawn(flush_usage_loop(gateway.clone(), usage_dir.clone()));

	axum::serve(listener, router(gateway.clone()))
		.with_graceful_shutdown(shutdown_signal())
		.await?;
	watcher.abort();
	flusher.abort();
	flush_usage(&gateway, &usage_dir).await;
	Ok(())
}

async fn shutdown_signal() {
	use tokio::signal::unix::{SignalKind, signal};
	let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
	tokio::select! {
		_ = term.recv() => {}
		_ = tokio::signal::ctrl_c() => {}
	}
}

async fn watch_config(gateway: Arc<Gateway>, path: PathBuf) {
	let mut loaded: Option<Vec<u8>> = None;
	loop {
		if let Ok(bytes) = tokio::fs::read(&path).await
			&& loaded.as_deref() != Some(&bytes)
		{
			match serde_json::from_slice::<GatewayConfig>(&bytes) {
				Ok(config) => {
					info!(keys = config.keys.len(), "gateway config loaded");
					gateway.set_config(config);
					loaded = Some(bytes);
				}
				Err(e) => warn!(error = %e, "gateway config is not valid; keeping the old one"),
			}
		}
		tokio::time::sleep(CONFIG_POLL).await;
	}
}

async fn flush_usage_loop(gateway: Arc<Gateway>, dir: PathBuf) {
	loop {
		tokio::time::sleep(USAGE_FLUSH).await;
		flush_usage(&gateway, &dir).await;
	}
}

/// Writes the counters collected so far as one file for the agent to collect.
pub async fn flush_usage(gateway: &Gateway, dir: &Path) {
	let rows = gateway.take_usage();
	if rows.is_empty() {
		return;
	}
	let nanos = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_nanos());
	let path = dir.join(format!("{nanos}.json"));
	let result = match serde_json::to_vec(&rows) {
		Ok(bytes) => write_atomic(&path, &bytes).await,
		Err(e) => Err(e.into()),
	};
	if let Err(e) = result {
		warn!(error = %e, "writing gateway usage failed; the counts are lost");
	}
}

#[cfg(test)]
mod tests {
	use proto::{GatewayKey, GatewayModel};

	use super::*;

	fn usage(requests: u64) -> GatewayUsage {
		GatewayUsage {
			key_hash: "k".to_owned(),
			model_id: 1,
			hour: 3600,
			requests,
			input_tokens: 10 * requests,
			output_tokens: 2 * requests,
		}
	}

	#[tokio::test]
	async fn usage_files_are_added_up_and_removed() {
		let dir = tempfile::tempdir().unwrap();
		let usage_dir = dir.path().join(USAGE_DIR);
		std::fs::create_dir_all(&usage_dir).unwrap();
		for (name, rows) in [("1.json", vec![usage(1)]), ("2.json", vec![usage(2)])] {
			std::fs::write(usage_dir.join(name), serde_json::to_vec(&rows).unwrap()).unwrap();
		}
		std::fs::write(usage_dir.join("3.tmp"), b"half written").unwrap();

		let rows = drain_usage(dir.path()).await;
		assert_eq!(rows, [usage(3)]);
		assert!(drain_usage(dir.path()).await.is_empty());
		assert!(
			usage_dir.join("3.tmp").exists(),
			"temp files are left alone"
		);
	}

	#[tokio::test]
	async fn config_is_written_once_per_change() {
		let dir = tempfile::tempdir().unwrap();
		let mut config = GatewayConfig {
			keys: vec![GatewayKey {
				key_hash: "h".to_owned(),
				models: vec![GatewayModel {
					model_id: 1,
					name: "m".to_owned(),
					host: "model-m".to_owned(),
					port: 8080,
					stream_usage: true,
				}],
			}],
			other_models: Vec::new(),
		};
		assert!(write_config(dir.path(), &config).await.unwrap());
		assert!(!write_config(dir.path(), &config).await.unwrap());
		assert_eq!(read_config(dir.path()).await, Some(config.clone()));
		config.keys.clear();
		assert!(write_config(dir.path(), &config).await.unwrap());
	}

	#[test]
	fn the_sidecar_is_on_the_podkit_network_under_its_alias() {
		let spec = container_spec(
			Path::new("/opt/podkit/podkit-agent"),
			"x@1",
			Path::new("/home/u/.podkit/gateway"),
		);
		assert_eq!(spec.network, "podkit");
		assert_eq!(spec.aliases, ["llm.podkit.internal"]);
		assert!(spec.published_ports.is_empty());
		assert_eq!(spec.binds[0], "/home/u/.podkit/gateway:/gw");
		assert_eq!(
			spec.binds[1],
			"/opt/podkit/podkit-agent:/opt/podkit/podkit-agent:ro"
		);
		let nix = container_spec(
			Path::new("/nix/store/a-agent/bin/podkit-agent"),
			"x",
			Path::new("/d"),
		);
		assert_eq!(nix.binds[1], "/nix/store:/nix/store:ro");
	}
}
