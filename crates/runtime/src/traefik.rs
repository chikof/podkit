use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::{
	ContainerId, ContainerSpec, ContainerState, ImageRef, PortMapping, Protocol, ResourceLimits,
	RestartPolicy,
};
use podkit_core::domain::server::Server;
use podkit_core::domain::shared::errors::DomainResult;

/// Podman network shared by Traefik and every app container on a server.
/// App containers publish no host ports, Traefik reaches them over this
/// network instead.
pub const NETWORK: &str = "podkit";

const TRAEFIK_CONTAINER_NAME: &str = "podkit-traefik";
const TRAEFIK_IMAGE: &str = "docker.io/library/traefik:v3.2";

/// ACME (Let's Encrypt) configuration for issuing real certs on custom
/// domains. Uses the HTTP-01 challenge, so it needs `https_port` reachable
/// from the internet on 443 (or whatever's forwarded to it) in addition to
/// the http entrypoint used for the challenge itself.
pub struct AcmeConfig<'a> {
	/// Contact email Let's Encrypt associates with issued certs.
	pub email: &'a str,
}

/// Ensures a per-server Traefik instance is running, configured with the
/// docker-label provider pointed at the same podman.sock `rt` itself talks
/// to (podman.sock is docker-API-compatible, so this works out of the box).
/// Idempotent: safe to call on every deploy.
///
/// `podman_socket_path` is the *host* path to bind-mount into the Traefik
/// container so it can reach the docker-compatible API itself. When `acme`
/// is `Some`, also opens `https_port` and configures a `le` certificate
/// resolver. Certs persist in a directory derived from `podman_socket_path`
/// (a sibling of the socket's own directory, which the operator already
/// proved is writable by registering the server at all).
///
/// # Errors
/// Returns an error if the network or container can't be created/started.
pub async fn ensure_traefik(
	rt: &dyn ContainerRuntime,
	podman_socket_path: &str,
	ingress_port: u16,
	https_port: u16,
	acme: Option<&AcmeConfig<'_>>,
) -> DomainResult<()> {
	rt.ensure_network(NETWORK).await?;

	let id = ContainerId(TRAEFIK_CONTAINER_NAME.to_string());
	match rt.inspect(&id).await {
		Ok(status) if status.state == ContainerState::Running => return Ok(()),
		Ok(_) => return rt.start_container(&id).await,
		Err(_) => {} // not found, create it below
	}

	rt.pull_image(&ImageRef(TRAEFIK_IMAGE.to_string())).await?;

	let mut command = vec![
		"--providers.docker=true".to_string(),
		"--providers.docker.endpoint=unix:///var/run/docker.sock".to_string(),
		"--providers.docker.exposedbydefault=false".to_string(),
		format!("--providers.docker.network={NETWORK}"),
		"--entrypoints.web.address=:80".to_string(),
	];
	let mut ports = vec![PortMapping {
		host_port: ingress_port,
		container_port: 80,
		protocol: Protocol::Tcp,
	}];
	let mut binds = vec![(
		podman_socket_path.to_string(),
		"/var/run/docker.sock".to_string(),
	)];

	if let Some(acme) = acme {
		command.push("--entrypoints.websecure.address=:443".to_string());
		command.push(format!(
			"--certificatesresolvers.le.acme.email={}",
			acme.email
		));
		command.push("--certificatesresolvers.le.acme.storage=/letsencrypt/acme.json".to_string());
		command.push("--certificatesresolvers.le.acme.httpchallenge=true".to_string());
		command.push("--certificatesresolvers.le.acme.httpchallenge.entrypoint=web".to_string());
		ports.push(PortMapping {
			host_port: https_port,
			container_port: 443,
			protocol: Protocol::Tcp,
		});
		binds.push((
			acme_storage_dir(podman_socket_path),
			"/letsencrypt".to_string(),
		));
	}

	let created = rt
		.create_container(ContainerSpec {
			name: TRAEFIK_CONTAINER_NAME.to_string(),
			image: ImageRef(TRAEFIK_IMAGE.to_string()),
			command: Some(command),
			env: vec![],
			ports,
			networks: vec![NETWORK.to_string()],
			labels: vec![],
			binds,
			resource_limits: ResourceLimits::default(),
			restart_policy: RestartPolicy::UnlessStopped,
		})
		.await?;

	rt.start_container(&created).await
}

/// Sibling of the podman socket's own directory, writable by construction
/// since the operator already proved that directory works by registering
/// the server, so it needs no separate per-server configuration.
fn acme_storage_dir(podman_socket_path: &str) -> String {
	std::path::Path::new(podman_socket_path)
		.parent()
		.and_then(|p| p.parent())
		.map(|p| p.join("podkit-letsencrypt"))
		.map_or_else(
			|| "/var/lib/podkit-letsencrypt".to_string(),
			|p| p.display().to_string(),
		)
}

/// Docker-provider labels that make Traefik route `hostname` to this
/// container on `container_port`. Router/service names are keyed by
/// `application_slug`, which is unique per server, the same scope as one
/// Traefik instance.
#[must_use]
pub fn routing_labels(
	application_slug: &str,
	hostname: &str,
	container_port: i32,
) -> Vec<(String, String)> {
	vec![
		("traefik.enable".to_string(), "true".to_string()),
		(
			format!("traefik.http.routers.{application_slug}.rule"),
			format!("Host(`{hostname}`)"),
		),
		(
			format!("traefik.http.routers.{application_slug}.entrypoints"),
			"web".to_string(),
		),
		(
			format!("traefik.http.services.{application_slug}.loadbalancer.server.port"),
			container_port.to_string(),
		),
	]
}

/// Labels for an HTTPS router over `custom_domains`, sharing the same
/// backend service the plain-HTTP router (`routing_labels`) declares.
/// Empty input yields no labels, so no router is created for an app with no
/// custom domains. Requires [`ensure_traefik`] to have been called with
/// `Some(acme)` for the `le` resolver referenced here to exist.
#[must_use]
pub fn secure_routing_labels(
	application_slug: &str,
	custom_domains: &[String],
) -> Vec<(String, String)> {
	if custom_domains.is_empty() {
		return vec![];
	}

	let rule = custom_domains
		.iter()
		.map(|d| format!("Host(`{d}`)"))
		.collect::<Vec<_>>()
		.join(" || ");
	let router = format!("{application_slug}-secure");

	vec![
		(format!("traefik.http.routers.{router}.rule"), rule),
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
			application_slug.to_string(),
		),
	]
}

/// Derives the zero-config public hostname for an app on `server`, via
/// sslip.io wildcard DNS. We don't own a wildcard domain ourselves in v1,
/// so this is the workaround.
///
/// For a local server this resolves to `127.0.0.1`, genuinely reachable
/// and testable on the same machine, not just a placeholder. For a remote
/// server, `server.hostname` is assumed to already be a bare IPv4 address.
/// Resolving DNS-name servers to an IP is a known gap for a later version.
#[must_use]
pub fn public_hostname(application_slug: &str, server: &Server) -> String {
	let dashed_ip = if server.is_local {
		"127-0-0-1".to_string()
	} else {
		server.hostname.replace('.', "-")
	};
	format!("{application_slug}.{dashed_ip}.sslip.io")
}

#[cfg(test)]
mod tests {
	use super::*;
	use podkit_core::domain::server::Server;
	use podkit_core::domain::shared::ids::{ServerId, TeamId};

	#[test]
	fn local_server_hostname_uses_loopback() {
		let server = Server::new_local(
			ServerId(1),
			TeamId(1),
			"local".to_string(),
			"localhost".to_string(),
			"/run/podman/podman.sock".to_string(),
		);
		assert_eq!(
			public_hostname("my-app", &server),
			"my-app.127-0-0-1.sslip.io"
		);
	}

	#[test]
	fn remote_server_hostname_dashes_the_ip() {
		let server = Server::new_remote(
			ServerId(1),
			TeamId(1),
			"remote".to_string(),
			"203.0.113.42".to_string(),
			22,
			"deploy".to_string(),
			vec![],
			"/run/podman/podman.sock".to_string(),
		);
		assert_eq!(
			public_hostname("my-app", &server),
			"my-app.203-0-113-42.sslip.io"
		);
	}

	#[test]
	fn routing_labels_key_by_slug() {
		let labels = routing_labels("my-app", "my-app.127-0-0-1.sslip.io", 3000);
		assert!(labels.contains(&("traefik.enable".to_string(), "true".to_string())));
		assert!(
			labels
				.iter()
				.any(|(k, v)| k == "traefik.http.routers.my-app.rule"
					&& v == "Host(`my-app.127-0-0-1.sslip.io`)")
		);
		assert!(labels.iter().any(|(k, v)| k
			== "traefik.http.services.my-app.loadbalancer.server.port"
			&& v == "3000"));
	}
}
