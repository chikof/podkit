//! The per-server Traefik instance that routes public traffic to containers.

use std::fmt::Write as _;
use std::path::Path;

use futures_util::StreamExt;
use proto::cidr::{self, Cidr};
use proto::{AccessRules, Ingress, PublishedPort};
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

/// Where Traefik serves its Prometheus metrics, inside its own container. The address is
/// loopback: no other container (not even one on a network Traefik joins) can reach it, and
/// no host port is published for it. The agent reads it with `wget` run in the container.
pub const METRICS_ADDRESS: &str = "127.0.0.1";
/// Port of the `metrics` entrypoint.
pub const METRICS_PORT: u16 = 8082;
/// Latency buckets in seconds; p50 and p95 are read off them, so they are finer than
/// Traefik's default (0.1, 0.3, 1.2, 5).
const LATENCY_BUCKETS: &str = "0.005,0.01,0.025,0.05,0.1,0.25,0.5,1,2.5,5,10";

/// The Traefik command line for `ingress`.
fn traefik_command(ingress: &Ingress) -> Vec<String> {
	let mut command = vec![
		"--providers.docker=true".to_string(),
		"--providers.docker.endpoint=unix:///var/run/docker.sock".to_string(),
		"--providers.docker.exposedbydefault=false".to_string(),
		"--entrypoints.web.address=:80".to_string(),
		// Prometheus metrics on a loopback-only entrypoint, per service (not per router).
		format!("--entrypoints.metrics.address={METRICS_ADDRESS}:{METRICS_PORT}"),
		"--metrics.prometheus=true".to_string(),
		"--metrics.prometheus.entrypoint=metrics".to_string(),
		"--metrics.prometheus.addentrypointslabels=false".to_string(),
		"--metrics.prometheus.addserviceslabels=true".to_string(),
		format!("--metrics.prometheus.buckets={LATENCY_BUCKETS}"),
	];
	if let Some(email) = &ingress.acme_email {
		command.push("--entrypoints.websecure.address=:443".to_string());
		command.push(format!("--certificatesresolvers.le.acme.email={email}"));
		command.push("--certificatesresolvers.le.acme.storage=/letsencrypt/acme.json".to_string());
		command.push("--certificatesresolvers.le.acme.httpchallenge=true".to_string());
		command.push("--certificatesresolvers.le.acme.httpchallenge.entrypoint=web".to_string());
	}
	command
}

/// The host ports Traefik publishes: HTTP, and HTTPS with an ACME email. Never the metrics
/// entrypoint.
fn published_ports(ingress: &Ingress) -> Vec<PublishedPort> {
	let mut ports = vec![PublishedPort::tcp(ingress.http_port, 80)];
	if ingress.acme_email.is_some() {
		ports.push(PublishedPort::tcp(ingress.https_port, 443));
	}
	ports
}

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

	let command = traefik_command(ingress);
	let published_ports = published_ports(ingress);
	let mut binds = vec![format!("{podman_socket}:/var/run/docker.sock")];
	if ingress.acme_email.is_some() {
		let acme_dir = data_dir.join("traefik-acme");
		tokio::fs::create_dir_all(&acme_dir).await?;
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

/// Container that is started to make Traefik re-read its routes.
pub const REFRESH_CONTAINER_NAME: &str = "podkit-ingress-refresh";

/// Makes Traefik look at the addresses of its routed containers again.
///
/// Traefik rebuilds its routes on container `start` and `die` events only;
/// it never hears of a container joining or leaving a network, so it keeps
/// the address the container had on a network it just left. Starting a
/// throwaway container (it exits at once, and is kept so the next start is
/// cheap) emits those events without restarting Traefik. Verified with Podman
/// 6 and Traefik 3: the stale route answers again within a second.
///
/// # Errors
/// Podman is unreachable or refuses to create or start the container.
pub async fn refresh(podman: &Podman) -> Result<(), PodmanError> {
	if podman.inspect(TRAEFIK_CONTAINER_NAME).await?.is_none() {
		return Ok(());
	}
	if podman.inspect(REFRESH_CONTAINER_NAME).await?.is_none() {
		podman
			.create_container(ContainerSpec {
				name: REFRESH_CONTAINER_NAME.to_string(),
				image: TRAEFIK_IMAGE.to_string(),
				entrypoint: Some(vec!["/bin/true".to_string()]),
				network: NETWORK.to_string(),
				..Default::default()
			})
			.await?;
	}
	podman.start(REFRESH_CONTAINER_NAME).await
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
		"{TRAEFIK_IMAGE}|{}|{}|{:?}|{podman_socket}|per-network-routing|metrics-v1|{LATENCY_BUCKETS}",
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

/// Sent when nobody may connect: a range no client address is in.
const NOBODY: &str = "255.255.255.255/32";

/// Seconds of HSTS the security-headers preset sends (one year).
const HSTS_SECONDS: u32 = 31_536_000;

/// Docker-provider labels for the middlewares of `access`, and for attaching them to the
/// routers of `routing_labels` (`service`) and `secure_routing_labels` (`service-secure`).
/// `http` and `https` say which of the two routers exist.
///
/// Order is the order they run: the IP list first (a refused client costs nothing more), then
/// the rate limit (so a password guesser is slowed before the login is checked), basic auth,
/// the body size, and the headers on the way out. The option names are Traefik's:
/// <https://doc.traefik.io/traefik/middlewares/http/ipallowlist/>, `ratelimit`,
/// `basicauth`, `buffering` and `headers` on the same site.
///
/// # Errors
/// A range of the rules does not parse.
pub fn access_labels(
	service: &str,
	access: &AccessRules,
	http: bool,
	https: bool,
) -> Result<Vec<(String, String)>, String> {
	let mut labels = Vec::new();
	let mut chain: Vec<String> = Vec::new();
	let mut add = |name: &str, option: &str, value: String| {
		labels.push((
			format!("traefik.http.middlewares.{service}-{name}.{option}"),
			value,
		));
	};
	if !access.allow.is_empty() || !access.deny.is_empty() {
		let ranges = cidr::effective_allow(&access.allow, &access.deny)?;
		let range = if ranges.is_empty() {
			NOBODY.to_string()
		} else {
			ranges
				.iter()
				.map(Cidr::to_string)
				.collect::<Vec<_>>()
				.join(",")
		};
		add("allow", "ipallowlist.sourcerange", range);
		chain.push(format!("{service}-allow"));
	}
	if let Some(average) = access.rate_average {
		add("rate", "ratelimit.average", average.to_string());
		add(
			"rate",
			"ratelimit.burst",
			access.rate_burst.unwrap_or(1).max(1).to_string(),
		);
		chain.push(format!("{service}-rate"));
	}
	if !access.basic_auth.is_empty() {
		add("auth", "basicauth.users", access.basic_auth.join(","));
		chain.push(format!("{service}-auth"));
	}
	if let Some(bytes) = access.max_body_bytes {
		add("body", "buffering.maxrequestbodybytes", bytes.to_string());
		chain.push(format!("{service}-body"));
	}
	if access.security_headers {
		add("headers", "headers.stsseconds", HSTS_SECONDS.to_string());
		add(
			"headers",
			"headers.stsincludesubdomains",
			"true".to_string(),
		);
		add("headers", "headers.framedeny", "true".to_string());
		add("headers", "headers.contenttypenosniff", "true".to_string());
		add(
			"headers",
			"headers.referrerpolicy",
			"strict-origin-when-cross-origin".to_string(),
		);
		chain.push(format!("{service}-headers"));
	}
	if chain.is_empty() {
		return Ok(labels);
	}
	let chain = chain.join(",");
	if http {
		labels.push((
			format!("traefik.http.routers.{service}.middlewares"),
			chain.clone(),
		));
	}
	if https {
		labels.push((
			format!("traefik.http.routers.{service}-secure.middlewares"),
			chain,
		));
	}
	Ok(labels)
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

	fn rules() -> AccessRules {
		AccessRules {
			allow: vec!["10.0.0.0/8".to_string()],
			rate_average: Some(5),
			rate_burst: Some(10),
			basic_auth: vec!["ann:$2y$10$abc".to_string(), "bob:$2y$10$def".to_string()],
			security_headers: true,
			max_body_bytes: Some(1_048_576),
			..AccessRules::default()
		}
	}

	fn get<'a>(labels: &'a [(String, String)], key: &str) -> Option<&'a str> {
		labels
			.iter()
			.find(|(k, _)| k == key)
			.map(|(_, v)| v.as_str())
	}

	#[test]
	fn access_labels_write_every_middleware_and_chain_them_in_order() {
		let labels = access_labels("app-7", &rules(), true, true).unwrap();
		let m = |k: &str| get(&labels, &format!("traefik.http.middlewares.app-7-{k}"));
		assert_eq!(m("allow.ipallowlist.sourcerange"), Some("10.0.0.0/8"));
		assert_eq!(m("rate.ratelimit.average"), Some("5"));
		assert_eq!(m("rate.ratelimit.burst"), Some("10"));
		assert_eq!(
			m("auth.basicauth.users"),
			Some("ann:$2y$10$abc,bob:$2y$10$def")
		);
		assert_eq!(m("body.buffering.maxrequestbodybytes"), Some("1048576"));
		assert_eq!(m("headers.headers.stsseconds"), Some("31536000"));
		assert_eq!(m("headers.headers.framedeny"), Some("true"));
		assert_eq!(m("headers.headers.contenttypenosniff"), Some("true"));
		assert_eq!(
			m("headers.headers.referrerpolicy"),
			Some("strict-origin-when-cross-origin")
		);
		let chain = "app-7-allow,app-7-rate,app-7-auth,app-7-body,app-7-headers";
		assert_eq!(
			get(&labels, "traefik.http.routers.app-7.middlewares"),
			Some(chain)
		);
		assert_eq!(
			get(&labels, "traefik.http.routers.app-7-secure.middlewares"),
			Some(chain)
		);
	}

	#[test]
	fn access_labels_only_attach_to_routers_that_exist() {
		let labels = access_labels("app-7", &rules(), true, false).unwrap();
		assert!(get(&labels, "traefik.http.routers.app-7.middlewares").is_some());
		assert!(get(&labels, "traefik.http.routers.app-7-secure.middlewares").is_none());
		let none = access_labels("app-7", &AccessRules::default(), true, true).unwrap();
		assert!(none.is_empty());
	}

	#[test]
	fn access_labels_each_option_alone() {
		let only = |r: AccessRules| access_labels("s", &r, true, false).unwrap();
		let rate = only(AccessRules {
			rate_average: Some(2),
			..AccessRules::default()
		});
		assert_eq!(
			get(&rate, "traefik.http.middlewares.s-rate.ratelimit.burst"),
			Some("1")
		);
		assert_eq!(
			get(&rate, "traefik.http.routers.s.middlewares"),
			Some("s-rate")
		);
		let body = only(AccessRules {
			max_body_bytes: Some(10),
			..AccessRules::default()
		});
		assert_eq!(
			get(&body, "traefik.http.routers.s.middlewares"),
			Some("s-body")
		);
		let auth = only(AccessRules {
			basic_auth: vec!["a:b".into()],
			..AccessRules::default()
		});
		assert_eq!(
			get(&auth, "traefik.http.routers.s.middlewares"),
			Some("s-auth")
		);
		let headers = only(AccessRules {
			security_headers: true,
			..AccessRules::default()
		});
		assert_eq!(
			get(&headers, "traefik.http.routers.s.middlewares"),
			Some("s-headers")
		);
	}

	#[test]
	fn access_labels_turn_a_deny_list_into_the_ranges_that_remain() {
		let deny = AccessRules {
			deny: vec!["10.0.0.128/25".into()],
			allow: vec!["10.0.0.0/24".into()],
			..AccessRules::default()
		};
		let labels = access_labels("s", &deny, true, false).unwrap();
		assert_eq!(
			get(
				&labels,
				"traefik.http.middlewares.s-allow.ipallowlist.sourcerange"
			),
			Some("10.0.0.0/25")
		);
		// Everything denied: nobody gets in, rather than everybody.
		let all = AccessRules {
			deny: vec!["0.0.0.0/0".into(), "::/0".into()],
			..AccessRules::default()
		};
		let labels = access_labels("s", &all, true, false).unwrap();
		assert_eq!(
			get(
				&labels,
				"traefik.http.middlewares.s-allow.ipallowlist.sourcerange"
			),
			Some(NOBODY)
		);
		let bad = AccessRules {
			allow: vec!["nope".into()],
			..AccessRules::default()
		};
		assert!(access_labels("s", &bad, true, false).is_err());
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
	fn traefik_metrics_are_not_exposed_publicly() {
		for acme in [None, Some("ops@example.com".to_string())] {
			let ingress = Ingress {
				acme_email: acme,
				..ingress(80)
			};
			let command = traefik_command(&ingress);
			// Prometheus is on, on its own entrypoint, bound to the container's loopback.
			assert!(command.contains(&"--metrics.prometheus=true".to_string()));
			assert!(command.contains(&"--metrics.prometheus.entrypoint=metrics".to_string()));
			assert!(command.contains(&format!(
				"--entrypoints.metrics.address={METRICS_ADDRESS}:{METRICS_PORT}"
			)));
			assert_eq!(METRICS_ADDRESS, "127.0.0.1");
			// Nothing publishes it: only the web and websecure ports leave the container.
			let ports = published_ports(&ingress);
			assert!(
				ports
					.iter()
					.all(|p| matches!(p.container, 80 | 443) && p.container != METRICS_PORT),
				"{ports:?}"
			);
			// The metrics entrypoint is not one of the public ones and the dashboard is off.
			assert!(!command.iter().any(|a| a.starts_with("--api")));
			assert!(
				command
					.iter()
					.filter(|a| a.contains(&format!(":{METRICS_PORT}")))
					.all(|a| a.contains(&format!("{METRICS_ADDRESS}:{METRICS_PORT}")))
			);
		}
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
