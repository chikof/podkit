//! Docker-compose files, as podkit runs them: parsed, interpolated and
//! checked against the supported subset of `docs/spikes/compose.md`.
//!
//! Parsing goes through `serde_json::Value` so every key is looked at and
//! decided on: supported, ignored with a warning, or refused. A refusal never
//! guesses. [`parse`] returns every [`Issue`] it found, each with the service
//! and the path of the key, instead of stopping at the first.

mod service;
pub mod value;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use proto::{ContainerOptions, Protocol};
use serde::Serialize;
use serde_json::Value;

pub use value::interpolate;

/// Largest compose file accepted, in bytes.
pub const MAX_BYTES: usize = 256 * 1024;
/// Most services in one stack.
pub const MAX_SERVICES: usize = 64;

/// Something in the file podkit will not run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
	/// The service the key belongs to; `None` for top-level keys.
	pub service: Option<String>,
	/// Dotted path of the key, e.g. `services.web.build`.
	pub path: String,
	/// The key itself, e.g. `build`.
	pub key: String,
	/// Why it is refused.
	pub message: String,
}

/// Something ignored or approximated; the stack still deploys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
	/// The service the key belongs to; `None` for top-level keys.
	pub service: Option<String>,
	/// Dotted path of the key.
	pub path: String,
	/// What happens instead.
	pub message: String,
}

/// How a service waits for a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Condition {
	/// The dependency has started.
	Started,
	/// The dependency's health check passes.
	Healthy,
}

/// A mount, after compose's many spellings are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
	/// A named volume of the stack. `name` is the compose-level name; the
	/// translation prefixes it with the stack.
	Volume {
		/// Compose-level volume name.
		name: String,
		/// Absolute path in the container.
		target: String,
		/// Mounted read-only.
		read_only: bool,
	},
	/// A file with known content, mounted read-only.
	File {
		/// Absolute path in the container.
		target: String,
		/// Content.
		content: String,
	},
}

/// One published port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
	/// Host port.
	pub host: u16,
	/// Container port.
	pub container: u16,
	/// TCP or UDP.
	pub protocol: Protocol,
	/// Host address; all addresses when `None`.
	pub host_ip: Option<String>,
}

/// One service of the compose file, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Service {
	/// Compose service name.
	pub name: String,
	/// Image reference.
	pub image: String,
	/// Replaces the image command.
	pub command: Option<Vec<String>>,
	/// Replaces the image entrypoint.
	pub entrypoint: Option<Vec<String>>,
	/// Environment, after interpolation and `env_file`.
	pub env: BTreeMap<String, String>,
	/// Container ports other services may reach (`expose` and every `ports`
	/// target); the first is a domain's default port.
	pub expose: BTreeSet<u16>,
	/// Ports published on the host.
	pub ports: Vec<PortMapping>,
	/// Volumes and files.
	pub mounts: Vec<Mount>,
	/// Services this one waits for.
	pub depends_on: BTreeMap<String, Condition>,
	/// Memory limit in bytes.
	pub memory_bytes: Option<i64>,
	/// CPU limit in cores.
	pub cpu_cores: Option<f64>,
	/// `/dev/shm` size in bytes.
	pub shm_bytes: Option<i64>,
	/// Seconds between SIGTERM and SIGKILL.
	pub stop_timeout_s: Option<u32>,
	/// Extra DNS names on the stack network (`container_name`, `hostname`,
	/// network aliases), besides the service name.
	pub aliases: BTreeSet<String>,
	/// Everything else the agent applies. `isolated`, `net_aliases` and `files`
	/// are left for the translation.
	pub options: ContainerOptions,
}

/// A validated compose file.
#[derive(Debug, Clone, PartialEq)]
pub struct Compose {
	/// Services by name.
	pub services: BTreeMap<String, Service>,
	/// Named volumes the services use (declared or implied), by compose name.
	pub volumes: BTreeSet<String>,
	/// Ignored or approximated keys.
	pub warnings: Vec<Warning>,
	/// Variables the file interpolates that the stack env does not set.
	pub missing_vars: BTreeSet<String>,
}

/// What the file is interpolated and resolved against.
pub struct Inputs<'a> {
	/// The stack's variables.
	pub env: &'a BTreeMap<String, String>,
	/// Files that relative binds, `env_file` and `configs` may name, by path
	/// relative to the compose file (`config/app.conf`). Templates supply
	/// these; raw stacks have none.
	pub files: &'a BTreeMap<String, String>,
}

/// Collects issues and warnings while walking the tree.
pub(crate) struct Ctx<'a> {
	pub(crate) inputs: &'a Inputs<'a>,
	pub(crate) issues: Vec<Issue>,
	pub(crate) warnings: Vec<Warning>,
	pub(crate) missing: BTreeSet<String>,
	/// Text of top-level `configs.*` and `secrets.*`, by that path.
	pub(crate) resources: BTreeMap<String, String>,
}

impl Ctx<'_> {
	pub(crate) fn issue(&mut self, service: Option<&str>, path: &str, message: impl Into<String>) {
		self.issues.push(Issue {
			service: service.map(str::to_string),
			path: path.to_string(),
			key: path.rsplit('.').next().unwrap_or(path).to_string(),
			message: message.into(),
		});
	}

	pub(crate) fn warn(&mut self, service: Option<&str>, path: &str, message: impl Into<String>) {
		let message = message.into();
		let service = service.map(str::to_string);
		let path = path.to_string();
		let duplicate = self
			.warnings
			.iter()
			.any(|w| w.service == service && w.path == path && w.message == message);
		if !duplicate {
			self.warnings.push(Warning {
				service,
				path,
				message,
			});
		}
	}
}

/// Interpolates every string value of `value` in place; a failing expression
/// is recorded as an issue at its path.
fn interpolate_tree(ctx: &mut Ctx<'_>, value: &mut Value, path: &str, service: Option<&str>) {
	match value {
		Value::String(s) => match interpolate(s, ctx.inputs.env, &mut ctx.missing) {
			Ok(expanded) => *s = expanded,
			Err(message) => ctx.issue(service, path, message),
		},
		Value::Array(items) => {
			for (i, item) in items.iter_mut().enumerate() {
				interpolate_tree(ctx, item, &join(path, &i.to_string()), service);
			}
		}
		Value::Object(map) => {
			for (key, item) in map.iter_mut() {
				let service = service.or_else(|| service_of(path, key));
				interpolate_tree(ctx, item, &join(path, key), service);
			}
		}
		_ => {}
	}
}

/// The service name when `key` is a direct child of `services`.
fn service_of<'a>(path: &str, key: &'a str) -> Option<&'a str> {
	(path == "services").then_some(key)
}

/// A compose service name: letters, digits, `_`, `.` and `-`, starting with a
/// letter or digit.
fn valid_service_name(name: &str) -> bool {
	let mut chars = name.chars();
	chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
		&& name.len() <= 63
		&& name
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// The service name as it appears in container and router names: lowercase
/// letters, digits and `-`.
#[must_use]
pub fn slug(name: &str) -> String {
	let slug: String = name
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() {
				c.to_ascii_lowercase()
			} else {
				'-'
			}
		})
		.collect();
	slug.trim_matches('-').to_string()
}

/// Parses `yaml`, interpolates it against `inputs.env` and checks it against
/// the supported subset.
///
/// # Errors
/// Every [`Issue`] found: YAML that does not parse, unsupported keys, bad
/// values, unknown dependencies and dependency cycles.
#[allow(clippy::too_many_lines, reason = "one pass over the top-level keys")]
pub fn parse(yaml: &str, inputs: &Inputs<'_>) -> Result<Compose, Vec<Issue>> {
	let top_issue = |message: String| {
		vec![Issue {
			service: None,
			path: String::new(),
			key: String::new(),
			message,
		}]
	};
	if yaml.len() > MAX_BYTES {
		return Err(top_issue(format!(
			"compose file is larger than {} KiB",
			MAX_BYTES / 1024
		)));
	}
	let mut root: Value =
		serde_saphyr::from_str(yaml).map_err(|e| top_issue(format!("not valid YAML: {e}")))?;
	let mut ctx = Ctx {
		inputs,
		issues: Vec::new(),
		warnings: Vec::new(),
		missing: BTreeSet::new(),
		resources: BTreeMap::new(),
	};
	let Some(top) = root.as_object_mut() else {
		return Err(top_issue(
			"a compose file must be a mapping with a `services` key".to_string(),
		));
	};
	// `x-*` blocks only exist to be referenced by anchors, which are resolved.
	top.retain(|key, _| !key.starts_with("x-"));
	interpolate_tree(&mut ctx, &mut root, "", None);
	let top = root.as_object().cloned().unwrap_or_default();

	let mut declared = BTreeSet::new();
	for (key, value) in &top {
		match key.as_str() {
			"services" | "version" | "name" => {}
			"volumes" => declared = top_volumes(&mut ctx, value),
			"networks" => top_networks(&mut ctx, value),
			"include" => ctx.issue(
				None,
				key,
				"include is not supported, paste the files into one",
			),
			"configs" | "secrets" => top_resources(&mut ctx, key, value),
			_ => ctx.issue(None, key, "unsupported key"),
		}
	}

	let services = match top.get("services") {
		Some(Value::Object(map)) if !map.is_empty() => map.clone(),
		Some(Value::Object(_) | Value::Null) | None => {
			ctx.issue(None, "services", "at least one service is required");
			serde_json::Map::new()
		}
		Some(_) => {
			ctx.issue(None, "services", "services must be a mapping");
			serde_json::Map::new()
		}
	};
	if services.len() > MAX_SERVICES {
		ctx.issue(
			None,
			"services",
			format!("a stack may have at most {MAX_SERVICES} services"),
		);
	}

	let mut parsed = BTreeMap::new();
	let mut slugs: BTreeMap<String, String> = BTreeMap::new();
	for (name, value) in &services {
		if !valid_service_name(name) {
			ctx.issue(
				Some(name),
				&format!("services.{name}"),
				"service names use letters, digits, `_`, `.` and `-`, and start with a letter or digit",
			);
			continue;
		}
		if let Some(other) = slugs.insert(slug(name), name.clone()) {
			ctx.issue(
				Some(name),
				&format!("services.{name}"),
				format!("service names {other:?} and {name:?} collide once lowercased"),
			);
			continue;
		}
		if let Some(service) = service::parse(&mut ctx, name, value, &declared) {
			parsed.insert(name.clone(), service);
		}
	}

	check_dependencies(&mut ctx, &parsed);
	let volumes = parsed
		.values()
		.flat_map(|s| s.mounts.iter())
		.filter_map(|m| match m {
			Mount::Volume { name, .. } => Some(name.clone()),
			Mount::File { .. } => None,
		})
		.collect();

	if !ctx.issues.is_empty() {
		return Err(ctx.issues);
	}
	let compose = Compose {
		services: parsed,
		volumes,
		warnings: ctx.warnings,
		missing_vars: ctx.missing,
	};
	Ok(compose)
}

fn top_volumes(ctx: &mut Ctx<'_>, value: &Value) -> BTreeSet<String> {
	let mut declared = BTreeSet::new();
	let Some(map) = value.as_object() else {
		if !value.is_null() {
			ctx.issue(None, "volumes", "volumes must be a mapping");
		}
		return declared;
	};
	for (name, spec) in map {
		declared.insert(name.clone());
		let path = format!("volumes.{name}");
		let Some(spec) = spec.as_object() else {
			continue;
		};
		for (key, value) in spec {
			let key_path = format!("{path}.{key}");
			match key.as_str() {
				"external" if value.as_bool() == Some(false) => {}
				"external" => ctx.issue(
					None,
					&key_path,
					"external volumes are not supported, use a volume of the stack",
				),
				"driver" if value.as_str() == Some("local") => {}
				"driver" => ctx.issue(None, &key_path, "only the local volume driver is supported"),
				"driver_opts" => ctx.issue(
					None,
					&key_path,
					"driver_opts are not supported (bind, nfs and tmpfs volumes)",
				),
				"name" | "labels" => ctx.warn(None, &key_path, "ignored"),
				_ if key.starts_with("x-") => {}
				_ => ctx.issue(None, &key_path, "unsupported key"),
			}
		}
	}
	declared
}

/// Top-level `configs` and `secrets`: named pieces of text that services
/// mount as files. The text comes from `content`, an environment variable of
/// the stack (`environment`) or a file the template supplies (`file`).
fn top_resources(ctx: &mut Ctx<'_>, kind: &str, value: &Value) {
	let Some(map) = value.as_object() else {
		if !value.is_null() {
			ctx.issue(None, kind, format!("{kind} must be a mapping"));
		}
		return;
	};
	for (name, spec) in map {
		let path = format!("{kind}.{name}");
		let Some(spec) = spec.as_object() else {
			ctx.issue(None, &path, format!("a {kind} entry must be a mapping"));
			continue;
		};
		let before = ctx.issues.len();
		let mut content = None;
		for (key, v) in spec {
			let key_path = format!("{path}.{key}");
			match (key.as_str(), v.as_str()) {
				("content", Some(text)) => content = Some(text.to_string()),
				("environment", Some(var)) => {
					content = Some(ctx.inputs.env.get(var).cloned().unwrap_or_default());
					if !ctx.inputs.env.contains_key(var) {
						ctx.missing.insert(var.to_string());
					}
				}
				("file", Some(file)) => {
					match ctx.inputs.files.get(&service::normalize_relative(file)) {
						Some(text) => content = Some(text.clone()),
						None => ctx.issue(
							None,
							&key_path,
							format!(
								"{file:?} is not a file of this stack; use content or environment"
							),
						),
					}
				}
				("external" | "driver" | "driver_opts" | "template_driver", _) => ctx.issue(
					None,
					&key_path,
					"external and driver-backed entries are not supported",
				),
				("name" | "labels", _) => ctx.warn(None, &key_path, "ignored"),
				_ if key.starts_with("x-") => {}
				_ => ctx.issue(None, &key_path, "unsupported key"),
			}
		}
		match content {
			Some(text) => {
				ctx.resources.insert(path, text);
			}
			None if ctx.issues.len() == before => {
				ctx.issue(None, &path, "needs content, environment or file");
			}
			None => {}
		}
	}
}

fn top_networks(ctx: &mut Ctx<'_>, value: &Value) {
	if value.as_object().is_some_and(|m| m.len() > 1) {
		ctx.warn(
			None,
			"networks",
			"every service joins one network of the stack; the declared networks are merged into it",
		);
	}
}

/// Dependencies must exist and must not form a cycle.
fn check_dependencies(ctx: &mut Ctx<'_>, services: &BTreeMap<String, Service>) {
	for (name, service) in services {
		for dep in service.depends_on.keys() {
			if !services.contains_key(dep) {
				ctx.issue(
					Some(name),
					&format!("services.{name}.depends_on.{dep}"),
					format!("depends on {dep:?}, which is not a service of this file"),
				);
			}
		}
	}
	if ctx.issues.iter().any(|i| i.path.contains(".depends_on.")) {
		return;
	}
	if let Err(cycle) = start_order(services) {
		let first = cycle.first().cloned().unwrap_or_default();
		ctx.issue(
			Some(&first),
			&format!("services.{first}.depends_on"),
			format!("dependency cycle: {}", cycle.join(" -> ")),
		);
	}
}

/// Service names in the order they must start: every service after the ones
/// it depends on, ties broken by name.
///
/// # Errors
/// The services of one dependency cycle, the first repeated at the end.
pub fn start_order(services: &BTreeMap<String, Service>) -> Result<Vec<String>, Vec<String>> {
	let mut remaining: BTreeMap<&str, BTreeSet<&str>> = services
		.iter()
		.map(|(name, s)| {
			(
				name.as_str(),
				s.depends_on
					.keys()
					.map(String::as_str)
					.filter(|d| services.contains_key(*d))
					.collect(),
			)
		})
		.collect();
	let mut order = Vec::with_capacity(services.len());
	while !remaining.is_empty() {
		let Some(next) = remaining
			.iter()
			.find(|(_, deps)| deps.is_empty())
			.map(|(name, _)| *name)
		else {
			return Err(find_cycle(&remaining));
		};
		remaining.remove(next);
		for deps in remaining.values_mut() {
			deps.remove(next);
		}
		order.push(next.to_string());
	}
	Ok(order)
}

/// Follows dependencies among the services that could not be ordered until a
/// name repeats.
fn find_cycle(remaining: &BTreeMap<&str, BTreeSet<&str>>) -> Vec<String> {
	let mut path: Vec<&str> = Vec::new();
	let mut current = remaining.keys().next().copied().unwrap_or_default();
	loop {
		if let Some(at) = path.iter().position(|p| *p == current) {
			let mut cycle: Vec<String> = path[at..].iter().map(|s| (*s).to_string()).collect();
			cycle.push(current.to_string());
			return cycle;
		}
		path.push(current);
		match remaining
			.get(current)
			.and_then(|deps| deps.iter().next().copied())
		{
			Some(next) => current = next,
			None => return path.iter().map(|s| (*s).to_string()).collect(),
		}
	}
}

impl Compose {
	/// Service names in start order. The file was checked for cycles in
	/// [`parse`], so this cannot fail.
	#[must_use]
	pub fn order(&self) -> Vec<String> {
		start_order(&self.services).unwrap_or_default()
	}
}

/// `path.key`, or just `key` at the top.
pub(crate) fn join(path: &str, key: &str) -> String {
	if path.is_empty() {
		key.to_string()
	} else {
		format!("{path}.{key}")
	}
}
