//! Talks to a team's servers: connecting to podman (local or over ssh),
//! cloning and building source, and keeping Traefik configured for ingress.

/// Connecting to a team's server, local or remote, as a usable runtime.
pub mod connection;
/// Errors shared across this crate.
pub mod error;
/// Cloning a git repo into a build context.
pub mod git;
/// Auto-detecting a build plan for a repo with nixpacks.
pub mod nixpacks;
/// The `ContainerRuntime` implementation backed by podman.
pub mod podman;
/// Managing the per-server Traefik instance that handles ingress.
pub mod traefik;
/// Forwarding a remote server's podman socket over ssh.
pub mod tunnel;

pub use connection::ServerConnection;
pub use error::RuntimeError;
pub use git::{ClonedSource, clone_to_tar};
pub use podman::{PodmanRuntime, local_socket_path};
pub use traefik::{
	AcmeConfig, ensure_traefik, public_hostname, routing_labels, secure_routing_labels,
};
pub use tunnel::{SshTarget, SshTunnel};
