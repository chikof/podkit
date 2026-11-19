//! The per-server Traefik instance that routes public traffic to containers.

use std::fmt::Write as _;
use std::path::Path;

use futures_util::StreamExt;
use proto::{Ingress, PublishedPort};
use sha2::{Digest, Sha256};

use crate::podman::{ContainerSpec, Podman, PodmanError};

/// Podman network of the system containers: Traefik and the gateway sidecar.
/// Apps, databases and models live on their environment's network instead.
pub const NETWORK: &str = "podkit";
/// Label naming the network Traefik reaches a routed container on.
pub const NETWORK_LABEL: &str = "traefik.docker.network";

/// Name of the Traefik container.
pub const TRAEFIK_CONTAINER_NAME: &str = "podkit-traefik";
/// Pinned Traefik image; `renovate.json` rewrites this literal.
pub const TRAEFIK_IMAGE: &str = "docker.io/library/traefik:v3.7.19";

const CONFIG_HASH_LABEL: &str = "podkit.config-hash";

/// Ensures Traefik runs with the configuration `ingress` describes, using the
/// docker-label provider on `podman_socket` (bind-mounted into the container).
/// Idempotent: safe to call on every deploy. A running Traefik whose
/// `podkit.config-hash` label differs from the wanted configuration is replaced.
///
/// ACME state persists in `<data_dir>/traefik-acme`.
///
/// # Errors
/// The network, image or container cannot be created or started.
pub async fn ensure_traefik(
	podman: &Podman,
	ingress: &Ingress,
	podman_socket: &str,
	data_dir: &Path,
) -> Result<(), PodmanError> {
	podman.ensure_network(NETWORK).await?;

	let hash = config_hash(ingress, podman_socket);
	if let Some(info) = podman.inspect(TRAEFIK_CONTAINER_NAME).await? {
		let current = info
			.config
			.and_then(|c| c.labels)
			.and_then(|mut l| l.remove(CONFIG_HASH_LABEL));
		if current.as_deref() == Some(hash.as_str()) {
			if !info.state.and_then(|s| s.running).unwrap_or(false) {
				podman.start(TRAEFIK_CONTAINER_NAME).await?;
			}
			return sync_networks(podman).await;
		}
		// Best effort: `remove` kills it anyway.
		let _ = podman.stop(TRAEFIK_CONTAINER_NAME).await;
		podman.remove(TRAEFIK_CONTAINER_NAME).await?;
	}

	if podman.inspect_image(TRAEFIK_IMAGE).await?.is_none() {
		let mut pull = std::pin::pin!(podman.pull_image(TRAEFIK_IMAGE));
		while let Some(progress) = pull.next().await {
			progress?;
		}
	}

	let mut command = vec![
		"--providers.docker=true".to_string(),
		"--providers.docker.endpoint=unix:///var/run/docker.sock".to_string(),
		"--providers.docker.exposedbydefault=false".to_string(),
		"--entrypoints.web.address=:80".to_string(),
	];
	let mut published_ports = vec![PublishedPort::tcp(ingress.http_port, 80)];
	let mut binds = vec![format!("{podman_socket}:/var/run/docker.sock")];

	if let Some(email) = &ingress.acme_email {
		let acme_dir = data_dir.join("traefik-acme");
		tokio::fs::create_dir_all(&acme_dir).await?;
		command.push("--entrypoints.websecure.address=:443".to_string());
		command.push(format!("--certificatesresolvers.le.acme.email={email}"));
		command.push("--certificatesresolvers.le.acme.storage=/letsencrypt/acme.json".to_string());
		command.push("--certificatesresolvers.le.acme.httpchallenge=true".to_string());
		command.push("--certificatesresolvers.le.acme.httpchallenge.entrypoint=web".to_string());
		published_ports.push(PublishedPort::tcp(ingress.https_port, 443));
		binds.push(format!("{}:/letsencrypt", acme_dir.display()));
	}

	podman
		.create_container(ContainerSpec {
			name: TRAEFIK_CONTAINER_NAME.to_string(),
			image: TRAEFIK_IMAGE.to_string(),
			command: Some(command),
			published_ports,
			network: NETWORK.to_string(),
			labels: vec![(CONFIG_HASH_LABEL.to_string(), hash)],
			binds,
			..Default::default()
		})
		.await?;
	podman.start(TRAEFIK_CONTAINER_NAME).await?;
	sync_networks(podman).await
}

/// Makes Traefik a member of exactly the networks that hold routed
/// containers (those with a `traefik.docker.network` label), plus the system
/// network. Idempotent; a missing Traefik is not an error.
///
/// # Errors
/// Podman is unreachable or refuses a connect or disconnect.
pub async fn sync_networks(podman: &Podman) -> Result<(), PodmanError> {
	let Some(info) = podman.inspect(TRAEFIK_CONTAINER_NAME).await? else {
		return Ok(());
	};
	let on: Vec<String> = info
		.network_settings
		.and_then(|n| n.networks)
		.map(|n| n.into_keys().collect())
		.unwrap_or_default();
	let want = wanted_networks(
		podman
			.labeled_containers("traefik.enable=true")
			.await?
			.iter()
			.filter_map(|c| {
				let networks = c
					.network_settings
					.as_ref()
					.and_then(|n| n.networks.as_ref())
					.map(|n| n.keys().map(String::as_str).collect::<Vec<_>>())
					.unwrap_or_default();
				routed_network(c.labels.as_ref()?.get(NETWORK_LABEL), &networks)
			}),
	);
	for name in &want {
		if !on.contains(name) {
			podman.create_network(name, false).await?;
			podman
				.connect_network(name, TRAEFIK_CONTAINER_NAME, &[])
				.await?;
		}
	}
	for name in &on {
		if name != NETWORK && !want.contains(name) {
			podman
				.disconnect_network(name, TRAEFIK_CONTAINER_NAME)
				.await?;
		}
	}
	Ok(())
}

/// The network Traefik reaches a routed container on: the one its label
/// names, else (a container deployed before the label existed) its
/// environment network.
fn routed_network(label: Option<&String>, networks: &[&str]) -> Option<String> {
	label.cloned().or_else(|| {
		networks
			.iter()
			.find(|n| n.starts_with(proto::ENV_NETWORK_PREFIX))
			.map(|n| (*n).to_owned())
	})
}

/// The networks Traefik should be on, given the network label of each routed
/// container: the labelled ones other than the system network, deduplicated.
fn wanted_networks(labels: impl Iterator<Item = String>) -> Vec<String> {
	let mut out: Vec<String> = labels.filter(|n| n != NETWORK).collect();
	out.sort();
	out.dedup();
	out
}

fn config_hash(ingress: &Ingress, podman_socket: &str) -> String {
	let input = format!(
		"{TRAEFIK_IMAGE}|{}|{}|{:?}|{podman_socket}|per-network-routing",
		ingress.http_port, ingress.https_port, ingress.acme_email
	);
	Sha256::digest(input.as_bytes())
		.iter()
		.fold(String::with_capacity(64), |mut out, b| {
			let _ = write!(out, "{b:02x}");
			out
		})
}

fn host_rule(hosts: &[String]) -> String {
	hosts
		.iter()
		.map(|h| format!("Host(`{h}`)"))
		.collect::<Vec<_>>()
		.join(" || ")
}

/// Docker-provider labels that route `http_hosts` on the `web` entrypoint to
/// `container_port`. Router and service names are keyed by `service`, so
/// containers of one service (during a rolling swap) share a load balancer.
#[must_use]
pub fn routing_labels(
	service: &str,
	http_hosts: &[String],
	container_port: u16,
) -> Vec<(String, String)> {
	let mut labels = vec![
		("traefik.enable".to_string(), "true".to_string()),
		(
			format!("traefik.http.services.{service}.loadbalancer.server.port"),
			container_port.to_string(),
		),
	];
	if !http_hosts.is_empty() {
		labels.push((
			format!("traefik.http.routers.{service}.rule"),
			host_rule(http_hosts),
		));
		labels.push((
			format!("traefik.http.routers.{service}.entrypoints"),
			"web".to_string(),
		));
		labels.push((
			format!("traefik.http.routers.{service}.service"),
			service.to_string(),
		));
	}
	labels
}

/// Labels for an HTTPS router over `https_hosts`, sharing the backend service
/// of [`routing_labels`]. Empty input yields no labels. Needs Traefik started
/// with an ACME email, which defines the `le` resolver.
#[must_use]
pub fn secure_routing_labels(service: &str, https_hosts: &[String]) -> Vec<(String, String)> {
	if https_hosts.is_empty() {
		return vec![];
	}
	let router = format!("{service}-secure");
	vec![
		(
			format!("traefik.http.routers.{router}.rule"),
			host_rule(https_hosts),
		),
		(
			format!("traefik.http.routers.{router}.entrypoints"),
			"websecure".to_string(),
		),
		(
			format!("traefik.http.routers.{router}.tls"),
			"true".to_string(),
		),
		(
			format!("traefik.http.routers.{router}.tls.certresolver"),
			"le".to_string(),
		),
		(
			format!("traefik.http.routers.{router}.service"),
			service.to_string(),
		),
	]
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ingress(http_port: u16) -> Ingress {
		Ingress {
			http_port,
			https_port: 443,
			acme_email: None,
		}
	}

	#[test]
	fn routing_labels_key_by_service() {
		let hosts = ["a.example.com".to_string(), "b.example.com".to_string()];
		let labels = routing_labels("app-7", &hosts, 3000);
		assert!(labels.contains(&("traefik.enable".to_string(), "true".to_string())));
		assert!(labels.contains(&(
			"traefik.http.routers.app-7.rule".to_string(),
			"Host(`a.example.com`) || Host(`b.example.com`)".to_string()
		)));
		assert!(labels.contains(&(
			"traefik.http.services.app-7.loadbalancer.server.port".to_string(),
			"3000".to_string()
		)));
	}

	#[test]
	fn traefik_joins_only_the_networks_of_routed_containers() {
		let want = wanted_networks(
			["podkit-env-2", "podkit", "podkit-env-1", "podkit-env-2"]
				.into_iter()
				.map(String::from),
		);
		assert_eq!(want, ["podkit-env-1", "podkit-env-2"]);
		assert!(wanted_networks(std::iter::empty()).is_empty());
	}

	#[test]
	fn unlabelled_routed_containers_are_reached_on_their_environment_network() {
		let label = "podkit-env-3".to_string();
		assert_eq!(
			routed_network(Some(&label), &["podkit-env-9"]).as_deref(),
			Some("podkit-env-3")
		);
		assert_eq!(
			routed_network(None, &["podkit", "podkit-env-9"]).as_deref(),
			Some("podkit-env-9")
		);
		assert_eq!(routed_network(None, &["podkit"]), None);
	}

	#[test]
	fn no_hosts_no_router() {
		assert_eq!(
			secure_routing_labels("app-7", &[]),
			Vec::<(String, String)>::new()
		);
		let labels = routing_labels("app-7", &[], 3000);
		assert!(labels.iter().all(|(k, _)| !k.contains(".routers.")));
	}

	#[test]
	fn config_hash_tracks_every_input() {
		let base = config_hash(&ingress(80), "/run/podman.sock");
		assert_eq!(base.len(), 64);
		assert_eq!(base, config_hash(&ingress(80), "/run/podman.sock"));
		assert_ne!(base, config_hash(&ingress(8080), "/run/podman.sock"));
		assert_ne!(base, config_hash(&ingress(80), "/other.sock"));
		let with_acme = Ingress {
			acme_email: Some("ops@example.com".to_string()),
			..ingress(80)
		};
		assert_ne!(base, config_hash(&with_acme, "/run/podman.sock"));
	}
}
