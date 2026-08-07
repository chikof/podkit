use async_trait::async_trait;

use crate::domain::shared::errors::DomainResult;

use super::entity::{BuildSpec, ContainerId, ContainerSpec, ContainerStatus, ImageRef, LogsQuery};

/// Contract owned by the domain, implemented in infrastructure (`crates/runtime`).
///
/// Every container operation MUST go through this trait: no bollard/podman
/// call may happen outside the infra impl of this trait. This keeps
/// `podkit-core` free of any infra dependency and lets a remote (SSH
/// tunneled) host reuse the same impl as the local host, differing only in
/// which socket the infra impl was constructed against.
#[async_trait]
pub trait ContainerRuntime: Send + Sync {
	/// Verifies the runtime is reachable and responding. Used as the
	/// connectivity probe when a remote server is registered.
	///
	/// # Errors
	/// Returns an error if the runtime cannot be reached.
	async fn ping(&self) -> DomainResult<()>;

	/// Builds an image from a tar build context, tagging it `spec.tag`.
	///
	/// # Errors
	/// Returns an error if the build context is malformed or the build fails.
	async fn build_image(&self, spec: BuildSpec) -> DomainResult<ImageRef>;

	/// Pulls an image by reference if not already present locally.
	///
	/// # Errors
	/// Returns an error if the image cannot be resolved or pulled.
	async fn pull_image(&self, image: &ImageRef) -> DomainResult<()>;

	/// Ensures a network named `name` exists. Idempotent, a no-op if
	/// already present. Used to give ingress (Traefik) and app containers a
	/// shared network to reach each other on, since app containers publish
	/// no host ports.
	///
	/// # Errors
	/// Returns an error if the network can't be created.
	async fn ensure_network(&self, name: &str) -> DomainResult<()>;

	/// Creates a container from `spec`. Does not start it.
	///
	/// # Errors
	/// Returns an error if creation fails (e.g. name conflict, missing image).
	async fn create_container(&self, spec: ContainerSpec) -> DomainResult<ContainerId>;

	/// Starts a previously created container.
	///
	/// # Errors
	/// Returns an error if the container cannot be started.
	async fn start_container(&self, id: &ContainerId) -> DomainResult<()>;

	/// Stops a running container.
	///
	/// # Errors
	/// Returns an error if the container cannot be stopped.
	async fn stop_container(&self, id: &ContainerId) -> DomainResult<()>;

	/// Removes a container. `force` kills it first if still running.
	///
	/// # Errors
	/// Returns an error if removal fails.
	async fn remove_container(&self, id: &ContainerId, force: bool) -> DomainResult<()>;

	/// Returns the container's current lifecycle state.
	///
	/// # Errors
	/// Returns an error if the container cannot be found or inspected.
	async fn inspect(&self, id: &ContainerId) -> DomainResult<ContainerStatus>;

	/// Fetches a buffered snapshot of log lines.
	///
	/// # Errors
	/// Returns an error if logs cannot be retrieved.
	async fn logs(&self, id: &ContainerId, query: LogsQuery) -> DomainResult<Vec<String>>;
}
