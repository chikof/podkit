//! `podkit-agent`: the per-server daemon the control plane talks to over a unix socket.

mod backup;
pub mod cron;
pub mod deploy;
pub mod gateway;
pub mod hardware;
pub mod health;
pub mod ingress;
pub mod model_download;
pub mod networks;
pub mod nixpacks;
pub mod podman;
pub mod probe;
pub mod registry;
pub mod routes;
pub mod scan;
pub mod source;
pub mod stats;
pub mod supervisor;

use std::path::PathBuf;

/// Shared state of every request handler.
pub struct Ctx {
	/// Connection to the Podman socket.
	pub docker: bollard::Docker,
	/// Agent state directory (`work/`, `backups/`, `traefik-acme/`, `desired.json`).
	pub data_dir: PathBuf,
	/// Host path of the Podman socket, bind-mounted into Traefik.
	pub podman_socket: String,
	/// Restarts desired containers when they die.
	pub supervisor: supervisor::Supervisor,
	/// HTTP client for dump uploads and restore downloads; has no total timeout.
	pub http: reqwest::Client,
}

impl Ctx {
	/// Container operations against this server's Podman.
	#[must_use]
	pub fn podman(&self) -> podman::Podman {
		podman::Podman::new(self.docker.clone())
	}
}
