//! Compose services as podkit [`DeploySpec`]s.
//!
//! One stack is one private network (`podkit-stack-<id>`) on which every
//! service answers to its compose name. A service with a domain also joins
//! the shared `podkit` network under a name that is unique to the stack, so
//! Traefik can reach it and nothing else of the stack is visible from there.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use proto::{
	DeploySpec, FileMount, Ingress, PublishedPort, Routing, Source, Strategy, VolumeMount,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::compose::{Compose, Condition, Issue, Mount, Service, slug};

/// Name of the network of stack `id`.
#[must_use]
pub fn network_name(stack: i64) -> String {
	format!("podkit-stack-{stack}")
}

/// Podman name of the stack volume `volume`.
#[must_use]
pub fn volume_name(stack: i64, volume: &str) -> String {
	format!("podkit-stack-{stack}-{volume}")
}

/// The agent's service name of compose service `name`; containers are
/// `podkit-<this>-<job id>`.
#[must_use]
pub fn service_name(stack: i64, name: &str) -> String {
	format!("stack-{stack}-{}", slug(name))
}

/// Public hostname of one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
	/// Hostname Traefik routes.
	pub host: String,
	/// Container port behind it; the service's only exposed port when `None`.
	pub port: Option<u16>,
}

/// What a plan depends on besides the compose file.
pub struct Settings<'a> {
	/// The stack's id.
	pub stack: i64,
	/// Traefik settings of the server, as in every deploy.
	pub ingress: Ingress,
	/// Domains by compose service name.
	pub domains: &'a BTreeMap<String, Domain>,
}

/// One service, ready to hand to the agent.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedService {
	/// Compose service name.
	pub name: String,
	/// The spec with `instance_id` 0; the deploy job sets the real one.
	pub spec: DeploySpec,
	/// Services to wait for before deploying this one.
	#[serde(skip)]
	pub depends_on: Vec<(String, Condition)>,
	/// Hash of `spec`: equal hashes mean the same container would come out.
	pub hash: String,
}

/// Everything a stack deploy does.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
	/// The stack network.
	pub network: String,
	/// Services in start order.
	pub services: Vec<PlannedService>,
	/// Podman names of the volumes the stack uses.
	pub volumes: Vec<String>,
}

/// Translates `compose` for the stack in `settings`.
///
/// # Errors
/// A domain naming no service or no usable port, and published ports that
/// collide with the ingress.
pub fn plan(compose: &Compose, settings: &Settings<'_>) -> Result<Plan, Vec<Issue>> {
	let stack = settings.stack;
	let mut issues = Vec::new();
	for name in settings.domains.keys() {
		if !compose.services.contains_key(name) {
			issues.push(issue(
				name,
				"domains",
				format!("domain set for {name:?}, which is not a service"),
			));
		}
	}

	let mut services = Vec::new();
	for name in compose.order() {
		let service = &compose.services[&name];
		match translate(stack, service, settings) {
			Ok(spec) => services.push(PlannedService {
				depends_on: service
					.depends_on
					.iter()
					.map(|(dep, condition)| (dep.clone(), *condition))
					.collect(),
				hash: hash(&spec),
				name,
				spec,
			}),
			Err(mut found) => issues.append(&mut found),
		}
	}
	if !issues.is_empty() {
		return Err(issues);
	}
	Ok(Plan {
		network: network_name(stack),
		services,
		volumes: compose
			.volumes
			.iter()
			.map(|v| volume_name(stack, v))
			.collect(),
	})
}

fn issue(service: &str, key: &str, message: String) -> Issue {
	Issue {
		service: Some(service.to_string()),
		path: format!("services.{service}.{key}"),
		key: key.to_string(),
		message,
	}
}

#[allow(clippy::too_many_lines, reason = "one field of the spec per block")]
fn translate(stack: i64, s: &Service, settings: &Settings<'_>) -> Result<DeploySpec, Vec<Issue>> {
	let service = service_name(stack, &s.name);
	let mut issues = Vec::new();

	let domain = settings.domains.get(&s.name);
	let container_port = domain.and_then(|d| match d.port {
		Some(port) => Some(port),
		None if s.expose.len() == 1 => s.expose.iter().next().copied(),
		None => {
			issues.push(issue(
				&s.name,
				"domain",
				format!(
					"{} needs a port for its domain: it exposes {}",
					s.name,
					if s.expose.is_empty() {
						"none".to_string()
					} else {
						s.expose
							.iter()
							.map(u16::to_string)
							.collect::<Vec<_>>()
							.join(", ")
					}
				),
			));
			None
		}
	});
	for port in &s.ports {
		if port.host == settings.ingress.http_port || port.host == settings.ingress.https_port {
			issues.push(issue(
				&s.name,
				"ports",
				format!("host port {} is used by the ingress", port.host),
			));
		}
	}
	if !issues.is_empty() {
		return Err(issues);
	}

	let routing = domain.map(|d| Routing {
		access: None,
		http_hosts: vec![d.host.clone()],
		https_hosts: if settings.ingress.acme_email.is_some() {
			vec![d.host.clone()]
		} else {
			Vec::new()
		},
	});
	let routed = routing.is_some();

	let mut options = s.options.clone();
	options.isolated = !routed;
	options.net_aliases = std::iter::once(s.name.clone())
		.chain(s.aliases.iter().cloned())
		.collect::<std::collections::BTreeSet<_>>()
		.into_iter()
		.collect();
	options.files = s
		.mounts
		.iter()
		.filter_map(|m| match m {
			Mount::File { target, content } => Some(FileMount {
				path: target.clone(),
				content: content.clone(),
			}),
			Mount::Volume { .. } => None,
		})
		.collect();

	let volumes: Vec<VolumeMount> = s
		.mounts
		.iter()
		.filter_map(|m| match m {
			Mount::Volume {
				name,
				target,
				read_only,
			} => Some(VolumeMount {
				name: volume_name(stack, name),
				path: target.clone(),
				read_only: *read_only,
			}),
			Mount::File { .. } => None,
		})
		.collect();
	let published_ports: Vec<PublishedPort> = s
		.ports
		.iter()
		.map(|p| PublishedPort {
			host: p.host,
			container: p.container,
			protocol: p.protocol,
			host_ip: p.host_ip.clone(),
		})
		.collect();
	// A volume or a host port can only be held by one container at a time.
	let strategy = if volumes.is_empty() && published_ports.is_empty() {
		Strategy::Rolling
	} else {
		Strategy::Recreate
	};

	Ok(DeploySpec {
		network_aliases: if routed {
			vec![format!("podkit-{service}")]
		} else {
			Vec::new()
		},
		service,
		instance_id: 0,
		source: Source::Image {
			reference: s.image.clone(),
			pull: true,
			auth: None,
		},
		env: s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
		command: s.command.clone(),
		container_port,
		routing,
		volumes,
		published_ports,
		memory_bytes: s.memory_bytes,
		cpu_cores: s.cpu_cores,
		strategy,
		ingress: settings.ingress.clone(),
		gpus: None,
		entrypoint: s.entrypoint.clone(),
		shm_bytes: s.shm_bytes,
		stop_timeout_s: s.stop_timeout_s,
		networks: vec![network_name(stack)],
		options,
		attach: Vec::new(),
		health: None,
	})
}

/// Gives every service its place on the networks beyond the stack's own.
/// A routed service lives on the stack network as its primary one, where
/// Traefik reaches it; every service also joins the networks the stack is
/// attached to (`linked`), under `podkit-<service>`. The specs' hashes change
/// with them, so attaching a stack redeploys its services.
pub fn link_networks(plan: &mut Plan, linked: &[crate::networks::attach::Linked]) {
	for planned in &mut plan.services {
		let spec = &mut planned.spec;
		let alias = format!("podkit-{}", spec.service);
		let mut attach = Vec::new();
		if spec.routing.is_some() {
			let mut aliases = vec![alias.clone()];
			aliases.extend(spec.options.net_aliases.iter().cloned());
			attach.push(proto::NetworkAttach {
				name: plan.network.clone(),
				aliases,
				internal: false,
			});
		}
		attach.extend(linked.iter().map(|l| proto::NetworkAttach {
			name: l.name.clone(),
			aliases: vec![alias.clone()],
			internal: l.internal,
		}));
		spec.attach = attach;
		planned.hash = hash(spec);
	}
}

/// SHA-256 of the spec as JSON, without the per-deploy instance id.
fn hash(spec: &DeploySpec) -> String {
	let mut spec = spec.clone();
	spec.instance_id = 0;
	let json = serde_json::to_vec(&spec).unwrap_or_default();
	Sha256::digest(&json)
		.iter()
		.fold(String::with_capacity(64), |mut out, b| {
			let _ = write!(out, "{b:02x}");
			out
		})
}
