//! Podman Quadlet units for what podkit runs: a [`DeploySpec`] as the `.container` unit that
//! starts the same container under systemd, with the `.network` and `.volume` units it needs.
//!
//! The units are a way out of podkit ("eject"), and a way to read what podkit runs. They come
//! from the very spec the agent deploys, so they match it, with three deliberate differences:
//!
//! - The environment never goes into a unit. It goes into an env file beside the units
//!   ([`Units::env`]) that the server only hands out in an explicit export.
//! - The container name has no deployment id (`podkit-app-1`, not `podkit-app-1-148`): systemd
//!   owns one stable container per unit.
//! - The `podkit.service` label is left out, so a podkit-agent on the same host does not take
//!   the container for its own and replace it on the next deploy.
//!
//! What has no Quadlet key (Traefik routing, the health gate's swap, rolling replicas, the MCP
//! bridge) is written as a comment in the unit, so nothing is dropped without a word.
//!
//! Every value is escaped for systemd: `%` doubles, and a value that needs it is quoted.

use std::fmt::Write as _;

use crate::{DeploySpec, GpuDevices, GpuVendor, HealthCheck, Healthcheck, Restart, Source};

/// Where the units go for a rootless Podman (the user podkit-agent runs as).
pub const ROOTLESS_DIR: &str = "~/.config/containers/systemd";
/// Where the units go for rootful Podman.
pub const ROOTFUL_DIR: &str = "/etc/containers/systemd";
/// The shared network of podkit's system containers, which a container without an
/// environment network lives on.
const SYSTEM_NETWORK: &str = crate::SYSTEM_NETWORK;

/// One generated file: its name in the unit directory and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
	/// Path relative to the unit directory, e.g. `podkit-app-1.container`.
	pub name: String,
	/// The file's content.
	pub content: String,
}

/// The units of one spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Units {
	/// The `.container` units: one, or one per replica.
	pub containers: Vec<File>,
	/// The `.network` units the containers join.
	pub networks: Vec<File>,
	/// The `.volume` units the containers mount.
	pub volumes: Vec<File>,
	/// The env file the containers read. Holds secret values: only for an explicit export.
	pub env: Option<File>,
	/// Files mounted into the container (a stack's configs), under `podkit-files/`. They can
	/// hold secrets too: only for an explicit export.
	pub mounted: Vec<File>,
	/// The variable names in [`Units::env`], for showing what the file holds without values.
	pub env_keys: Vec<String>,
	/// The systemd services of the containers, `podkit-app-1.service`, in start order.
	pub services: Vec<String>,
}

impl Units {
	/// The files that hold no secret value: networks, volumes, then containers.
	pub fn public(&self) -> impl Iterator<Item = &File> {
		self.networks
			.iter()
			.chain(&self.volumes)
			.chain(&self.containers)
	}

	/// The files that may hold secrets: the env file and the mounted files.
	pub fn secret(&self) -> impl Iterator<Item = &File> {
		self.env.iter().chain(&self.mounted)
	}
}

/// What the spec alone does not say.
#[derive(Debug, Clone, Default)]
pub struct Options<'a> {
	/// A line for the unit's `Description=`, e.g. `podkit application web`.
	pub description: &'a str,
	/// The podkit services (`stack-3-db`) this one starts after: a compose `depends_on`.
	pub after: &'a [String],
}

/// The unit name (without suffix) of a podkit service: `app-1` becomes `podkit-app-1`.
#[must_use]
pub fn unit_base(service: &str) -> String {
	format!("podkit-{service}")
}

/// The container unit of replica `index` (0 is the first) of `service`.
fn container_base(service: &str, index: usize) -> String {
	if index == 0 {
		unit_base(service)
	} else {
		format!("{}-r{}", unit_base(service), index + 1)
	}
}

/// The units that run `spec` under systemd.
#[must_use]
pub fn generate(spec: &DeploySpec, options: &Options<'_>) -> Units {
	let base = unit_base(&spec.service);
	let env_name = format!("{base}.env");
	let env = (!spec.env.is_empty()).then(|| env_file(spec));
	let env_keys: Vec<String> = spec.env.iter().map(|(k, _)| k.clone()).collect();
	let mounted: Vec<File> = spec
		.options
		.files
		.iter()
		.enumerate()
		.map(|(i, f)| File {
			name: mounted_name(&spec.service, i, &f.path),
			content: f.content.clone(),
		})
		.collect();

	let placement = placement(spec);
	let networks = placement
		.iter()
		.map(|n| network_unit(&n.name, n.internal))
		.collect();
	let volumes = spec.volumes.iter().map(|v| volume_unit(&v.name)).collect();

	let count = spec.replicas.map_or(1, |r| r.count.max(1) as usize);
	let mut containers = Vec::with_capacity(count);
	let mut services = Vec::with_capacity(count);
	for index in 0..count {
		let name = container_base(&spec.service, index);
		containers.push(File {
			name: format!("{name}.container"),
			content: container_unit(
				spec,
				options,
				&Ctx {
					name: &name,
					index,
					count,
					env: env.as_ref().map(|_| env_name.as_str()),
					keys: &env_keys,
					placement: &placement,
					mounted: &mounted,
				},
			),
		});
		services.push(format!("{name}.service"));
	}

	Units {
		containers,
		networks,
		volumes,
		env,
		mounted,
		env_keys,
		services,
	}
}

/// Where a mounted file goes, relative to the unit directory.
fn mounted_name(service: &str, index: usize, path: &str) -> String {
	let base: String = path
		.rsplit('/')
		.next()
		.unwrap_or("file")
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
				c
			} else {
				'_'
			}
		})
		.collect();
	let base = if base.is_empty() || base.chars().all(|c| c == '.') {
		"file".to_owned()
	} else {
		base
	};
	format!("podkit-files/{service}/{}-{base}", index + 1)
}

/// The env file: `KEY=value` lines, as `podman --env-file` reads them (literally, no quotes).
fn env_file(spec: &DeploySpec) -> File {
	let mut out = String::from("# Environment of podkit service ");
	out.push_str(&spec.service);
	out.push_str(". Holds secrets: keep it mode 0600.\n");
	for (key, value) in &spec.env {
		if value.contains('\n') || value.contains('\r') {
			let _ = writeln!(
				out,
				"# {key} has a value of several lines, which an env file cannot hold; set it another way"
			);
			continue;
		}
		let _ = writeln!(out, "{key}={value}");
	}
	File {
		name: format!("{}.env", unit_base(&spec.service)),
		content: out,
	}
}

/// A network a container is on, with its aliases there.
struct Joined {
	name: String,
	aliases: Vec<String>,
	internal: bool,
}

/// The networks of the container, the one it lives on first: what the agent's
/// `container_spec` picks.
fn placement(spec: &DeploySpec) -> Vec<Joined> {
	let joined = |name: &str, aliases: &[String], internal: bool| Joined {
		name: name.to_owned(),
		aliases: aliases.to_vec(),
		internal,
	};
	let mut out: Vec<Joined> = Vec::new();
	if spec.options.isolated {
		for n in &spec.networks {
			out.push(joined(n, &spec.options.net_aliases, false));
		}
		for a in &spec.attach {
			out.push(joined(&a.name, &a.aliases, a.internal));
		}
	} else if spec.attach.is_empty() {
		out.push(joined(SYSTEM_NETWORK, &spec.network_aliases, false));
		for n in &spec.networks {
			out.push(joined(n, &spec.options.net_aliases, false));
		}
	} else {
		for a in &spec.attach {
			out.push(joined(&a.name, &a.aliases, a.internal));
		}
		for n in spec
			.networks
			.iter()
			.filter(|n| !spec.attach.iter().any(|a| &a.name == *n))
		{
			out.push(joined(n, &spec.options.net_aliases, false));
		}
	}
	// One entry per network, the first place it was named.
	let mut seen = Vec::new();
	out.retain(|j| {
		if seen.contains(&j.name) {
			false
		} else {
			seen.push(j.name.clone());
			true
		}
	});
	out
}

fn network_unit(name: &str, internal: bool) -> File {
	let mut out = String::new();
	header(
		&mut out,
		&format!("Podman network {name}, as podkit creates it."),
	);
	out.push_str("[Network]\n");
	key(&mut out, "NetworkName", name);
	if internal {
		out.push_str("# No outbound access, as in podkit.\n");
		key(&mut out, "Internal", "true");
	}
	File {
		name: format!("{name}.network"),
		content: out,
	}
}

fn volume_unit(name: &str) -> File {
	let mut out = String::new();
	header(
		&mut out,
		&format!(
			"Podman volume {name}. An existing volume of that name (podkit's) is used as it is."
		),
	);
	out.push_str("[Volume]\n");
	key(&mut out, "VolumeName", name);
	File {
		name: format!("{name}.volume"),
		content: out,
	}
}

fn header(out: &mut String, what: &str) {
	out.push_str("# Generated by podkit. ");
	out.push_str(&one_line(what));
	out.push('\n');
	out.push('\n');
}

/// What one container unit needs beyond the spec.
struct Ctx<'a> {
	name: &'a str,
	index: usize,
	count: usize,
	env: Option<&'a str>,
	keys: &'a [String],
	placement: &'a [Joined],
	mounted: &'a [File],
}

#[allow(
	clippy::too_many_lines,
	reason = "one unit, key by key in the order Quadlet documents them"
)]
fn container_unit(spec: &DeploySpec, options: &Options<'_>, ctx: &Ctx<'_>) -> String {
	let mut out = String::new();
	header(
		&mut out,
		&format!("The container of podkit service {}.", spec.service),
	);
	if ctx.count > 1 {
		let _ = writeln!(
			out,
			"# Replica {} of {}. podkit balances them behind Traefik; here each is its own unit.",
			ctx.index + 1,
			ctx.count
		);
	}

	// [Unit]
	out.push_str("[Unit]\n");
	let description = if options.description.is_empty() {
		format!("podkit {}", spec.service)
	} else {
		options.description.to_owned()
	};
	key(&mut out, "Description", &description);
	for dep in options.after {
		let unit = format!("{}.service", unit_base(dep));
		key(&mut out, "Requires", &unit);
		key(&mut out, "After", &unit);
	}
	out.push('\n');

	// [Container]
	out.push_str("[Container]\n");
	image_keys(&mut out, spec);
	key(&mut out, "ContainerName", ctx.name);
	out.push_str("# podkit labels the container podkit.service=");
	out.push_str(&one_line(&spec.service));
	out.push_str("; left out so podkit-agent does not take it for its own.\n");
	for (k, v) in &spec.options.labels {
		key(&mut out, "Label", &quote_if_needed(&format!("{k}={v}")));
	}

	// Command line.
	if let Some(bridge) = &spec.options.mcp_bridge {
		let _ = writeln!(
			out,
			"# podkit serves this stdio MCP server over HTTP on port {} at {} through podkit-agent's bridge; Quadlet has no such key, so the image's own command runs.",
			bridge.port,
			one_line(&bridge.path)
		);
		if !bridge.command.is_empty() {
			let _ = writeln!(
				out,
				"# MCP command: {}",
				one_line(&exec_line(&bridge.command))
			);
		}
	}
	if let Some(entrypoint) = &spec.entrypoint {
		key(&mut out, "Entrypoint", &entrypoint_value(entrypoint));
	}
	if let Some(command) = &spec.command {
		key(&mut out, "Exec", &exec_line(command));
	}

	// Environment: names only; the values are in the env file.
	if let Some(env) = ctx.env {
		let _ = writeln!(
			out,
			"# Variables, their values in {env}: {}",
			one_line(&ctx.keys.join(", "))
		);
		key(&mut out, "EnvironmentFile", env);
	}

	// Networks.
	for joined in ctx.placement {
		let mut value = format!("{}.network", joined.name);
		if !joined.aliases.is_empty() {
			value.push(':');
			value.push_str(
				&joined
					.aliases
					.iter()
					.map(|a| format!("alias={a}"))
					.collect::<Vec<_>>()
					.join(","),
			);
		}
		key(&mut out, "Network", &value);
	}

	// Routing has no Quadlet key.
	if let Some(routing) = &spec.routing {
		let hosts: Vec<&str> = routing
			.http_hosts
			.iter()
			.chain(&routing.https_hosts)
			.map(String::as_str)
			.collect();
		let _ = writeln!(
			out,
			"# podkit routes {} to port {} through Traefik; nothing here does. Publish a port or put a reverse proxy in front.",
			if hosts.is_empty() {
				"no host".to_owned()
			} else {
				one_line(&hosts.join(", "))
			},
			spec.container_port
				.map_or_else(|| "?".to_owned(), |p| p.to_string())
		);
		if routing.access.as_ref().is_some_and(|a| !a.is_empty()) {
			out.push_str("# The access rules (IP lists, rate limit, basic auth, headers) are Traefik middlewares and do not apply.\n");
		}
	}

	for port in &spec.published_ports {
		let mut value = String::new();
		if let Some(ip) = &port.host_ip {
			if ip.contains(':') {
				let _ = write!(value, "[{ip}]:");
			} else {
				let _ = write!(value, "{ip}:");
			}
		}
		let _ = write!(value, "{}:{}", port.host, port.container);
		if port.protocol == crate::Protocol::Udp {
			value.push_str("/udp");
		}
		if ctx.index > 0 {
			let _ = writeln!(
				out,
				"# PublishPort={value} is on the first replica only: two cannot bind one host port."
			);
		} else {
			key(&mut out, "PublishPort", &value);
		}
	}

	// Storage.
	for volume in &spec.volumes {
		let mode = if volume.read_only { ":ro" } else { "" };
		key(
			&mut out,
			"Volume",
			&format!("{}.volume:{}{mode}", volume.name, volume.path),
		);
	}
	for (file, mount) in ctx.mounted.iter().zip(&spec.options.files) {
		key(
			&mut out,
			"Volume",
			&format!("./{}:{}:ro", file.name, mount.path),
		);
	}
	for path in &spec.options.tmpfs {
		key(&mut out, "Tmpfs", path);
	}

	// Health.
	health_keys(&mut out, spec);

	// Devices.
	if let Some(gpus) = &spec.gpus {
		match gpu_devices(gpus.vendor, &gpus.devices) {
			Some(devices) => {
				for device in devices {
					key(&mut out, "AddDevice", &device);
				}
			}
			None => out
				.push_str("# The GPU request has no device form; add AddDevice= lines by hand.\n"),
		}
	}

	// Process and security.
	if let Some(user) = &spec.options.user {
		match user.split_once(':') {
			Some((user, group)) => {
				key(&mut out, "User", user);
				key(&mut out, "Group", group);
			}
			None => key(&mut out, "User", user),
		}
	}
	if let Some(dir) = &spec.options.working_dir {
		key(&mut out, "WorkingDir", dir);
	}
	if let Some(host) = &spec.options.hostname {
		key(&mut out, "HostName", host);
	}
	if spec.options.read_only {
		key(&mut out, "ReadOnly", "true");
	}
	if spec.options.init {
		key(&mut out, "RunInit", "true");
	}
	for host in &spec.options.extra_hosts {
		key(&mut out, "AddHost", host);
	}
	for dns in &spec.options.dns {
		key(&mut out, "DNS", dns);
	}
	for cap in &spec.options.cap_add {
		key(&mut out, "AddCapability", cap);
	}
	for cap in &spec.options.cap_drop {
		key(&mut out, "DropCapability", cap);
	}
	for (k, v) in &spec.options.sysctls {
		key(&mut out, "Sysctl", &format!("{k}={v}"));
	}
	for u in &spec.options.ulimits {
		key(
			&mut out,
			"Ulimit",
			&format!("{}={}:{}", u.name, u.soft, u.hard),
		);
	}
	if let Some(s) = spec.stop_timeout_s {
		key(&mut out, "StopTimeout", &s.to_string());
	}

	// Limits and the rest, through PodmanArgs.
	if let Some(bytes) = spec.memory_bytes {
		key(&mut out, "PodmanArgs", &format!("--memory={bytes}"));
	}
	if let Some(cores) = spec.cpu_cores {
		key(&mut out, "PodmanArgs", &format!("--cpus={cores}"));
	}
	if let Some(bytes) = spec.shm_bytes {
		key(&mut out, "PodmanArgs", &format!("--shm-size={bytes}"));
	}
	for group in &spec.options.group_add {
		key(&mut out, "PodmanArgs", &format!("--group-add={group}"));
	}
	out.push_str(
		"# podkit logs to a 10 MB k8s-file; under systemd the journal has the logs (journalctl).\n",
	);
	out.push('\n');

	// [Service]
	out.push_str("[Service]\n");
	match spec.options.restart {
		Restart::Always => key(&mut out, "Restart", "always"),
		Restart::Never => key(&mut out, "Restart", "no"),
	}
	out.push_str("# Room for the image pull on the first start.\n");
	key(&mut out, "TimeoutStartSec", "900");
	out.push('\n');

	// [Install]
	out.push_str("[Install]\n");
	key(&mut out, "WantedBy", "default.target");
	out
}

/// `Image=`, with `Pull=` and `AutoUpdate=` where they follow from the source.
fn image_keys(out: &mut String, spec: &DeploySpec) {
	match &spec.source {
		Source::Image {
			reference,
			pull,
			auth,
		} => {
			if auth.is_some() {
				let host = auth
					.as_ref()
					.and_then(|a| a.server.clone())
					.unwrap_or_else(|| image_host(reference).to_owned());
				let _ = writeln!(
					out,
					"# Pulled with a saved registry login; run `podman login {}` first.",
					one_line(&host)
				);
			}
			key(out, "Image", reference);
			if *pull {
				if auto_updatable(reference) {
					key(out, "AutoUpdate", "registry");
				}
			} else {
				out.push_str(
					"# The image is on this server already (podkit built or pulled it); it is not pulled again.\n",
				);
				key(out, "Pull", "never");
			}
		}
		Source::Git {
			repo_url, git_ref, ..
		} => {
			let tag = format!("localhost/{}:latest", unit_base(&spec.service));
			let _ = writeln!(
				out,
				"# podkit builds this image from {} ({}) on every deploy. Build it yourself and tag it {tag}.",
				one_line(&scrub_url(repo_url)),
				one_line(git_ref)
			);
			key(out, "Image", &tag);
			key(out, "Pull", "never");
		}
	}
}

/// The health keys: the deploy's health gate wins over a compose healthcheck, as in the agent.
fn health_keys(out: &mut String, spec: &DeploySpec) {
	if let Some(gate) = &spec.health {
		let timeouts = [
			("HealthInterval", duration(gate.interval_ms)),
			("HealthTimeout", duration(gate.timeout_ms)),
			("HealthRetries", gate.retries.to_string()),
			("HealthStartPeriod", duration(gate.start_period_ms)),
		];
		match &gate.check {
			HealthCheck::Command { command } => {
				key(out, "HealthCmd", command);
				for (k, v) in timeouts {
					key(out, k, &v);
				}
			}
			HealthCheck::Http { path, status, port } => {
				let _ = writeln!(
					out,
					"# podkit checks GET {} on port {port} for status {status} with podkit-agent mounted in the container. Quadlet has no HTTP check: pick a command the image has, for example:",
					one_line(path)
				);
				let _ = writeln!(
					out,
					"# HealthCmd=wget -q -O /dev/null http://127.0.0.1:{port}{}",
					one_line(path).replace('%', "%%")
				);
				for (k, v) in timeouts {
					let _ = writeln!(out, "# {k}={v}");
				}
			}
		}
		let _ = writeln!(
			out,
			"# The deploy waited up to {} s for health and drained the old container for {} s; systemd does neither.",
			gate.gate_timeout_s, gate.drain_s
		);
		return;
	}
	if let Some(check) = &spec.options.healthcheck {
		healthcheck_keys(out, check);
	}
}

fn healthcheck_keys(out: &mut String, check: &Healthcheck) {
	let cmd = match check.test.first().map(String::as_str) {
		Some("NONE") => "none".to_owned(),
		Some("CMD-SHELL") => check.test.get(1).cloned().unwrap_or_default(),
		Some(_) => json_array(&check.test),
		None => return,
	};
	key(out, "HealthCmd", &cmd);
	if let Some(ms) = check.interval_ms {
		key(out, "HealthInterval", &duration(ms));
	}
	if let Some(ms) = check.timeout_ms {
		key(out, "HealthTimeout", &duration(ms));
	}
	if let Some(n) = check.retries {
		key(out, "HealthRetries", &n.to_string());
	}
	if let Some(ms) = check.start_period_ms {
		key(out, "HealthStartPeriod", &duration(ms));
	}
}

/// The devices of a GPU request, as the agent maps them; `None` for a request the agent
/// refuses too.
fn gpu_devices(vendor: GpuVendor, devices: &GpuDevices) -> Option<Vec<String>> {
	match (vendor, devices) {
		(GpuVendor::Nvidia, GpuDevices::All) => Some(vec!["nvidia.com/gpu=all".to_owned()]),
		(GpuVendor::Nvidia, GpuDevices::Count(n)) if *n > 0 => {
			Some((0..*n).map(|i| format!("nvidia.com/gpu={i}")).collect())
		}
		(GpuVendor::Nvidia, GpuDevices::Ids(ids)) if !ids.is_empty() => Some(
			ids.iter()
				.map(|id| format!("nvidia.com/gpu={id}"))
				.collect(),
		),
		(GpuVendor::Amd, GpuDevices::All) => {
			Some(vec!["/dev/kfd".to_owned(), "/dev/dri".to_owned()])
		}
		(GpuVendor::Amd, GpuDevices::Ids(ids)) if !ids.is_empty() => {
			let mut out = vec!["/dev/kfd".to_owned()];
			out.extend(ids.iter().map(|id| format!("/dev/dri/{id}")));
			Some(out)
		}
		_ => None,
	}
}

/// Whether `podman auto-update` can follow `reference`: a fully qualified name with a tag,
/// not a digest.
fn auto_updatable(reference: &str) -> bool {
	if reference.contains('@') {
		return false;
	}
	let Some((host, _)) = reference.split_once('/') else {
		return false;
	};
	host.contains('.') || host.contains(':') || host == "localhost"
}

/// The registry host of an image reference; `docker.io` for a short one.
fn image_host(reference: &str) -> &str {
	match reference.split_once('/') {
		Some((host, _)) if host.contains('.') || host.contains(':') || host == "localhost" => host,
		_ => "docker.io",
	}
}

/// `url` without a `user:password@` part.
fn scrub_url(url: &str) -> String {
	if let Some((scheme, rest)) = url.split_once("://") {
		let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
		let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
		if path.is_empty() && !rest.contains('/') {
			return format!("{scheme}://{host}");
		}
		return format!("{scheme}://{host}/{path}");
	}
	// scp form, `git@host:path`: the user is not a secret.
	url.to_owned()
}

/// `30s`, `500ms`: Podman's duration form.
fn duration(ms: u64) -> String {
	if ms.is_multiple_of(1000) {
		format!("{}s", ms / 1000)
	} else {
		format!("{ms}ms")
	}
}

/// One `Key=value` line. A value with a line break cannot be a unit value: it becomes a
/// comment that says so.
fn key(out: &mut String, name: &str, value: &str) {
	if value.contains('\n') || value.contains('\r') {
		let _ = writeln!(
			out,
			"# {name}= has a value of several lines, which a unit cannot hold; set it by hand."
		);
		return;
	}
	let _ = writeln!(out, "{name}={}", value.replace('%', "%%"));
}

/// Text for a comment: one line.
fn one_line(s: &str) -> String {
	s.replace(['\n', '\r'], " ")
}

/// A word for a systemd command line: quoted when it has to be.
fn quote_if_needed(word: &str) -> String {
	let plain = !word.is_empty()
		&& word
			.chars()
			.all(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '\\' | ';'));
	if plain {
		return word.to_owned();
	}
	let mut out = String::with_capacity(word.len() + 2);
	out.push('"');
	for c in word.chars() {
		match c {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			c => out.push(c),
		}
	}
	out.push('"');
	out
}

/// `Exec=`: the words of a command, each quoted when it has to be.
fn exec_line(words: &[String]) -> String {
	words
		.iter()
		.map(|w| quote_if_needed(w))
		.collect::<Vec<_>>()
		.join(" ")
}

/// `Entrypoint=`: one word as it is, several as a JSON array, which Podman accepts.
fn entrypoint_value(words: &[String]) -> String {
	match words {
		[one] => one.clone(),
		_ => json_array(words),
	}
}

fn json_array(words: &[String]) -> String {
	let mut out = String::from("[");
	for (i, w) in words.iter().enumerate() {
		if i > 0 {
			out.push(',');
		}
		out.push('"');
		for c in w.chars() {
			match c {
				'"' => out.push_str("\\\""),
				'\\' => out.push_str("\\\\"),
				'\n' => out.push_str("\\n"),
				'\r' => out.push_str("\\r"),
				'\t' => out.push_str("\\t"),
				c if (c as u32) < 0x20 => {
					let _ = write!(out, "\\u{:04x}", c as u32);
				}
				c => out.push(c),
			}
		}
		out.push('"');
	}
	out.push(']');
	out
}

/// What the README of an export says about one resource.
#[derive(Debug, Clone)]
pub struct ReadmeEntry<'a> {
	/// `application web`, `database main` …
	pub label: &'a str,
	/// Its units.
	pub units: &'a Units,
}

/// The README of an export: how to install the units, the order they start in, and what
/// podkit no longer does for them. `skipped` lists resources that could not be exported, with
/// the reason.
#[must_use]
pub fn readme(
	title: &str,
	entries: &[ReadmeEntry<'_>],
	with_secrets: bool,
	skipped: &[String],
) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "# Quadlet units: {}\n", one_line(title));
	out.push_str(
		"podkit generated these Podman Quadlet units from the same settings it deploys with.\n\
		They run the containers under systemd, without podkit.\n\n",
	);

	out.push_str("## Install\n\n");
	out.push_str("Rootless (as the user that should own the containers):\n\n```sh\n");
	let _ = writeln!(out, "mkdir -p {ROOTLESS_DIR}");
	let _ = writeln!(out, "cp -r units/. {ROOTLESS_DIR}/");
	if with_secrets {
		let _ = writeln!(out, "chmod 600 {ROOTLESS_DIR}/*.env");
	}
	out.push_str("systemctl --user daemon-reload\n");
	out.push_str(
		"loginctl enable-linger \"$USER\"   # keep them running after you log out\n```\n\n",
	);
	let _ = writeln!(
		out,
		"Rootful: copy them to `{ROOTFUL_DIR}/` instead and drop `--user` below.\n"
	);
	out.push_str(
		"Quadlet units cannot be `systemctl enable`d: the `[Install]` section of each\n\
		`.container` unit already starts it at boot.\n\n",
	);
	if !with_secrets {
		out.push_str(
			"This export has no env files. Each unit that reads one names it in\n\
			`EnvironmentFile=` and lists the variable names above it: create the file\n\
			beside the unit (`KEY=value` per line, mode 0600) before starting it.\n\n",
		);
	}

	out.push_str(
		"## Start\n\nIn this order (networks and volumes are created on the way):\n\n```sh\n",
	);
	for entry in entries {
		for service in &entry.units.services {
			let _ = writeln!(
				out,
				"systemctl --user start {service}   # {}",
				one_line(entry.label)
			);
		}
	}
	out.push_str("```\n\n");
	out.push_str(
		"Stop podkit's own containers of the same resources first (or delete the\n\
		resources in podkit), or both copies fight over ports and volumes. The volumes\n\
		keep their podkit names, so a unit started on the same server finds the data.\n\n",
	);

	out.push_str("## What stops working\n\n");
	out.push_str("podkit does none of this for containers started from these units:\n\n");
	for line in [
		"Traefik routing: the generated domains, custom domains, HTTPS certificates and access rules (IP lists, rate limits, basic auth). Publish ports or run your own reverse proxy.",
		"Deploys: git pushes, webhooks, registry webhooks, previews, rollbacks and the build of git applications. Images are not rebuilt; `AutoUpdate=registry` units can follow a moving tag with `podman auto-update`.",
		"Health-gated and rolling deploys, and load balancing over replicas: each replica is a separate unit.",
		"Backups of databases and volumes, and their restores.",
		"Cron jobs, uptime checks, notifications, metrics, monitoring, log search, the web terminal and image scanning.",
		"The model gateway, MCP gateway and its stdio bridge, and Tailscale sidecars.",
		"Variable changes: shared and per-resource variables were resolved at export time. Change the env files by hand.",
		"podkit-agent's supervisor: systemd restarts the containers (`Restart=always`) instead.",
	] {
		let _ = writeln!(out, "- {line}");
	}
	out.push('\n');

	if !skipped.is_empty() {
		out.push_str("## Not exported\n\n");
		for line in skipped {
			let _ = writeln!(out, "- {}", one_line(line));
		}
		out.push('\n');
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		AccessRules, ContainerOptions, FileMount, HealthGate, Ingress, NetworkAttach,
		PublishedPort, RegistryAuth, Replicas, Routing, Strategy, VolumeMount,
	};

	fn spec() -> DeploySpec {
		DeploySpec {
			service: "app-1".into(),
			instance_id: 148,
			source: Source::Image {
				reference: "ghcr.io/acme/web:1.2".into(),
				pull: true,
				auth: None,
			},
			env: vec![
				("DATABASE_URL".into(), "postgres://u:s3cret@db/app".into()),
				("MODE".into(), "prod".into()),
			],
			command: None,
			container_port: Some(3000),
			routing: None,
			network_aliases: Vec::new(),
			volumes: Vec::new(),
			published_ports: Vec::new(),
			memory_bytes: None,
			cpu_cores: None,
			strategy: Strategy::Rolling,
			ingress: Ingress {
				http_port: 80,
				https_port: 443,
				acme_email: None,
			},
			gpus: None,
			entrypoint: None,
			shm_bytes: None,
			stop_timeout_s: None,
			networks: Vec::new(),
			options: ContainerOptions::default(),
			attach: Vec::new(),
			health: None,
			replicas: None,
			auto_update: false,
		}
	}

	fn units_of(spec: &DeploySpec) -> Units {
		generate(spec, &Options::default())
	}

	fn container(units: &Units) -> &str {
		&units.containers[0].content
	}

	fn lines(text: &str) -> Vec<&str> {
		text.lines().collect()
	}

	#[test]
	fn a_container_unit_has_the_image_name_restart_and_install_sections() {
		let units = units_of(&spec());
		assert_eq!(units.containers.len(), 1);
		assert_eq!(units.containers[0].name, "podkit-app-1.container");
		assert_eq!(units.services, ["podkit-app-1.service"]);
		let unit = container(&units);
		let l = lines(unit);
		for want in [
			"[Unit]",
			"Description=podkit app-1",
			"[Container]",
			"Image=ghcr.io/acme/web:1.2",
			"ContainerName=podkit-app-1",
			"AutoUpdate=registry",
			"[Service]",
			"Restart=always",
			"[Install]",
			"WantedBy=default.target",
		] {
			assert!(l.contains(&want), "missing {want:?} in\n{unit}");
		}
		// The sections come in this order.
		let at = |s: &str| unit.find(s).unwrap();
		assert!(at("[Unit]") < at("[Container]"));
		assert!(at("[Container]") < at("[Service]"));
		assert!(at("[Service]") < at("[Install]"));
	}

	#[test]
	fn env_values_never_reach_the_unit_only_the_env_file() {
		let units = units_of(&spec());
		let unit = container(&units);
		assert!(!unit.contains("s3cret"), "{unit}");
		assert!(!unit.contains("Environment="), "{unit}");
		assert!(
			lines(unit).contains(&"EnvironmentFile=podkit-app-1.env"),
			"{unit}"
		);
		assert!(unit.contains("# Variables, their values in podkit-app-1.env: DATABASE_URL, MODE"));
		let env = units.env.as_ref().unwrap();
		assert_eq!(env.name, "podkit-app-1.env");
		assert!(
			env.content
				.contains("DATABASE_URL=postgres://u:s3cret@db/app\n")
		);
		assert!(env.content.contains("MODE=prod\n"));
		assert_eq!(units.env_keys, ["DATABASE_URL", "MODE"]);
		// The public files hold no value; the secret ones are exactly the env file.
		assert!(units.public().all(|f| !f.content.contains("s3cret")));
		assert_eq!(units.secret().count(), 1);
	}

	#[test]
	fn no_env_means_no_env_file_and_no_environment_file_key() {
		let mut s = spec();
		s.env.clear();
		let units = units_of(&s);
		assert!(units.env.is_none());
		assert!(!container(&units).contains("EnvironmentFile"));
	}

	#[test]
	fn a_multi_line_value_is_left_out_of_the_env_file_with_a_note() {
		let mut s = spec();
		s.env = vec![("KEY".into(), "-----BEGIN\nabc\n-----END".into())];
		let env = units_of(&s).env.unwrap().content;
		assert!(!env.contains("abc"), "{env}");
		assert!(env.contains("# KEY has a value of several lines"), "{env}");
	}

	#[test]
	fn the_podkit_service_label_is_left_out_but_other_labels_stay() {
		let mut s = spec();
		s.options.labels = vec![
			("com.example.team".into(), "web".into()),
			("note".into(), "two words".into()),
		];
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			!lines(&unit)
				.iter()
				.any(|l| l.starts_with("Label=podkit.service"))
		);
		assert!(
			lines(&unit).contains(&"Label=com.example.team=web"),
			"{unit}"
		);
		assert!(lines(&unit).contains(&"Label=\"note=two words\""), "{unit}");
	}

	#[test]
	fn the_environment_network_comes_first_with_its_aliases_and_gets_a_network_unit() {
		let mut s = spec();
		s.attach = vec![
			NetworkAttach {
				name: "podkit-env-3".into(),
				aliases: vec!["web".into(), "web.internal".into()],
				internal: false,
			},
			NetworkAttach {
				name: "podkit-net-9".into(),
				aliases: Vec::new(),
				internal: true,
			},
		];
		let units = units_of(&s);
		let unit = container(&units);
		let nets: Vec<&str> = lines(unit)
			.into_iter()
			.filter(|l| l.starts_with("Network="))
			.collect();
		assert_eq!(
			nets,
			[
				"Network=podkit-env-3.network:alias=web,alias=web.internal",
				"Network=podkit-net-9.network",
			]
		);
		assert_eq!(units.networks.len(), 2);
		assert_eq!(units.networks[0].name, "podkit-env-3.network");
		assert!(
			units.networks[0]
				.content
				.contains("[Network]\nNetworkName=podkit-env-3\n")
		);
		assert!(!units.networks[0].content.contains("Internal="));
		assert!(units.networks[1].content.contains("Internal=true"));
	}

	#[test]
	fn without_an_environment_network_the_container_lives_on_podkit() {
		let mut s = spec();
		s.network_aliases = vec!["podkit-db-1".into()];
		let units = units_of(&s);
		assert!(lines(container(&units)).contains(&"Network=podkit.network:alias=podkit-db-1"));
		assert_eq!(units.networks[0].name, "podkit.network");
	}

	#[test]
	fn an_isolated_stack_service_lives_on_its_stack_network_only() {
		let mut s = spec();
		s.options.isolated = true;
		s.networks = vec!["podkit-stack-4".into()];
		s.options.net_aliases = vec!["db".into()];
		let units = units_of(&s);
		let unit = container(&units);
		assert!(lines(unit).contains(&"Network=podkit-stack-4.network:alias=db"));
		assert!(!unit.contains("Network=podkit.network"), "{unit}");
		assert_eq!(units.networks.len(), 1);
	}

	#[test]
	fn volumes_get_volume_units_and_keep_their_podman_names() {
		let mut s = spec();
		s.volumes = vec![
			VolumeMount {
				name: "podkit-db-1-data".into(),
				path: "/var/lib/postgresql/data".into(),
				read_only: false,
			},
			VolumeMount {
				name: "models".into(),
				path: "/models".into(),
				read_only: true,
			},
		];
		let units = units_of(&s);
		let l = lines(container(&units));
		assert!(l.contains(&"Volume=podkit-db-1-data.volume:/var/lib/postgresql/data"));
		assert!(l.contains(&"Volume=models.volume:/models:ro"));
		assert_eq!(units.volumes[0].name, "podkit-db-1-data.volume");
		assert!(
			units.volumes[0]
				.content
				.contains("[Volume]\nVolumeName=podkit-db-1-data\n")
		);
	}

	#[test]
	fn ports_keep_protocol_and_host_address() {
		let mut s = spec();
		s.published_ports = vec![
			PublishedPort::tcp(5432, 5432),
			PublishedPort {
				host: 53,
				container: 53,
				protocol: crate::Protocol::Udp,
				host_ip: Some("127.0.0.1".into()),
			},
			PublishedPort {
				host: 8080,
				container: 80,
				protocol: crate::Protocol::Tcp,
				host_ip: Some("::1".into()),
			},
		];
		let l = units_of(&s).containers.remove(0).content;
		let l = lines(&l);
		assert!(l.contains(&"PublishPort=5432:5432"));
		assert!(l.contains(&"PublishPort=127.0.0.1:53:53/udp"));
		assert!(l.contains(&"PublishPort=[::1]:8080:80"));
	}

	#[test]
	fn limits_go_through_podman_args() {
		let mut s = spec();
		s.memory_bytes = Some(512 * 1024 * 1024);
		s.cpu_cores = Some(1.5);
		s.shm_bytes = Some(1 << 30);
		s.stop_timeout_s = Some(30);
		let unit = units_of(&s).containers.remove(0).content;
		let l = lines(&unit);
		assert!(l.contains(&"PodmanArgs=--memory=536870912"), "{unit}");
		assert!(l.contains(&"PodmanArgs=--cpus=1.5"), "{unit}");
		assert!(l.contains(&"PodmanArgs=--shm-size=1073741824"), "{unit}");
		assert!(l.contains(&"StopTimeout=30"), "{unit}");
	}

	#[test]
	fn a_command_health_gate_becomes_health_keys() {
		let mut s = spec();
		s.health = Some(HealthGate {
			check: HealthCheck::Command {
				command: "pg_isready -U app".into(),
			},
			interval_ms: 5000,
			timeout_ms: 1500,
			retries: 3,
			start_period_ms: 10_000,
			gate_timeout_s: 120,
			drain_s: 5,
		});
		let unit = units_of(&s).containers.remove(0).content;
		let l = lines(&unit);
		assert!(l.contains(&"HealthCmd=pg_isready -U app"), "{unit}");
		assert!(l.contains(&"HealthInterval=5s"));
		assert!(l.contains(&"HealthTimeout=1500ms"));
		assert!(l.contains(&"HealthRetries=3"));
		assert!(l.contains(&"HealthStartPeriod=10s"));
		assert!(unit.contains("# The deploy waited up to 120 s"), "{unit}");
	}

	#[test]
	fn an_http_health_gate_is_a_comment_because_it_needs_the_agent() {
		let mut s = spec();
		s.health = Some(HealthGate {
			check: HealthCheck::Http {
				path: "/healthz".into(),
				status: 200,
				port: 3000,
			},
			interval_ms: 5000,
			timeout_ms: 2000,
			retries: 3,
			start_period_ms: 0,
			gate_timeout_s: 60,
			drain_s: 0,
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			!lines(&unit).iter().any(|l| l.starts_with("Health")),
			"{unit}"
		);
		assert!(unit.contains("# HealthCmd=wget -q -O /dev/null http://127.0.0.1:3000/healthz"));
		assert!(unit.contains("# HealthInterval=5s"));
	}

	#[test]
	fn a_compose_healthcheck_keeps_its_form() {
		let mut s = spec();
		s.options.healthcheck = Some(Healthcheck {
			test: vec![
				"CMD".into(),
				"curl".into(),
				"-f".into(),
				"http://localhost/".into(),
			],
			interval_ms: Some(30_000),
			timeout_ms: None,
			retries: Some(5),
			start_period_ms: None,
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			lines(&unit).contains(&r#"HealthCmd=["CMD","curl","-f","http://localhost/"]"#),
			"{unit}"
		);
		assert!(lines(&unit).contains(&"HealthInterval=30s"));
		assert!(lines(&unit).contains(&"HealthRetries=5"));

		s.options.healthcheck = Some(Healthcheck {
			test: vec!["CMD-SHELL".into(), "test -f /ready".into()],
			interval_ms: None,
			timeout_ms: None,
			retries: None,
			start_period_ms: None,
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(lines(&unit).contains(&"HealthCmd=test -f /ready"), "{unit}");
	}

	#[test]
	fn routing_has_no_key_and_is_said_in_a_comment() {
		let mut s = spec();
		s.routing = Some(Routing {
			http_hosts: vec!["web.example.test".into()],
			https_hosts: vec!["shop.example.com".into()],
			access: Some(AccessRules {
				allow: vec!["10.0.0.0/8".into()],
				..AccessRules::default()
			}),
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			unit.contains(
				"# podkit routes web.example.test, shop.example.com to port 3000 through Traefik"
			),
			"{unit}"
		);
		assert!(unit.contains("# The access rules"), "{unit}");
		assert!(!unit.contains("traefik."), "{unit}");
	}

	#[test]
	fn replicas_are_one_unit_each_and_only_the_first_publishes_ports() {
		let mut s = spec();
		s.replicas = Some(Replicas {
			count: 3,
			max_surge: 1,
			max_unavailable: 0,
			sticky: false,
		});
		s.published_ports = vec![PublishedPort::tcp(8080, 3000)];
		let units = units_of(&s);
		let names: Vec<&str> = units.containers.iter().map(|f| f.name.as_str()).collect();
		assert_eq!(
			names,
			[
				"podkit-app-1.container",
				"podkit-app-1-r2.container",
				"podkit-app-1-r3.container"
			]
		);
		assert!(lines(&units.containers[0].content).contains(&"PublishPort=8080:3000"));
		assert!(lines(&units.containers[1].content).contains(&"ContainerName=podkit-app-1-r2"));
		assert!(!lines(&units.containers[1].content).contains(&"PublishPort=8080:3000"));
		assert!(units.containers[1].content.contains("# Replica 2 of 3."));
		// One env file for all of them.
		assert!(lines(&units.containers[2].content).contains(&"EnvironmentFile=podkit-app-1.env"));
	}

	#[test]
	fn git_sources_name_a_local_tag_and_never_the_credentials() {
		let mut s = spec();
		s.source = Source::Git {
			repo_url: "https://x-access-token:ghp_secret@github.com/acme/web.git".into(),
			git_ref: "main".into(),
			commit: None,
			auth: crate::GitAuth::HttpsToken {
				token: "ghp_secret".into(),
				username: None,
			},
			build: crate::Build::Nixpacks,
			options: crate::BuildOptions::default(),
		};
		let unit = units_of(&s).containers.remove(0).content;
		assert!(!unit.contains("ghp_secret"), "{unit}");
		assert!(
			unit.contains("https://github.com/acme/web.git (main)"),
			"{unit}"
		);
		assert!(lines(&unit).contains(&"Image=localhost/podkit-app-1:latest"));
		assert!(lines(&unit).contains(&"Pull=never"));
	}

	#[test]
	fn a_registry_login_is_named_never_written() {
		let mut s = spec();
		s.source = Source::Image {
			reference: "registry.example.com/team/api:3".into(),
			pull: true,
			auth: Some(RegistryAuth {
				username: "bot".into(),
				password: "hunter2".into(),
				server: Some("registry.example.com".into()),
			}),
		};
		let unit = units_of(&s).containers.remove(0).content;
		assert!(!unit.contains("hunter2"), "{unit}");
		assert!(
			unit.contains("`podman login registry.example.com`"),
			"{unit}"
		);
	}

	#[test]
	fn auto_update_only_for_fully_qualified_tags() {
		assert!(auto_updatable("ghcr.io/a/b:1"));
		assert!(auto_updatable("localhost:5000/b:1"));
		assert!(!auto_updatable("postgres:16"));
		assert!(!auto_updatable("docker.io/library/postgres@sha256:abc"));
		let mut s = spec();
		s.source = Source::Image {
			reference: "postgres:16".into(),
			pull: true,
			auth: None,
		};
		assert!(!container(&units_of(&s)).contains("AutoUpdate"));
		s.source = Source::Image {
			reference: "podkit-app-1:abc123".into(),
			pull: false,
			auth: None,
		};
		let units = units_of(&s);
		assert!(lines(container(&units)).contains(&"Pull=never"));
	}

	#[test]
	fn command_and_entrypoint_are_quoted_and_percent_escaped() {
		let mut s = spec();
		s.command = Some(vec![
			"serve".into(),
			"--name".into(),
			"my model".into(),
			"--fmt=%d".into(),
			"say \"hi\"".into(),
		]);
		s.entrypoint = Some(vec!["/bin/sh".into(), "-c".into()]);
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			lines(&unit).contains(&r#"Exec=serve --name "my model" --fmt=%%d "say \"hi\"""#),
			"{unit}"
		);
		assert!(
			lines(&unit).contains(&r#"Entrypoint=["/bin/sh","-c"]"#),
			"{unit}"
		);
		s.entrypoint = Some(vec!["/init".into()]);
		assert!(lines(container(&units_of(&s))).contains(&"Entrypoint=/init"));
	}

	#[test]
	fn a_value_with_a_line_break_cannot_inject_keys() {
		let mut s = spec();
		s.options.working_dir = Some("/app\nExecStartPre=/bin/evil".into());
		s.options.hostname = Some("h\r\nUser=root".into());
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			!lines(&unit).iter().any(|l| l.starts_with("ExecStartPre")),
			"{unit}"
		);
		assert!(!lines(&unit).contains(&"User=root"), "{unit}");
		assert!(unit.contains("# WorkingDir= has a value of several lines"));
	}

	#[test]
	fn mounted_files_are_secret_files_bound_read_only_beside_the_units() {
		let mut s = spec();
		s.options.files = vec![FileMount {
			path: "/etc/nginx/conf.d/site conf".into(),
			content: "server {}".into(),
		}];
		let units = units_of(&s);
		assert_eq!(units.mounted[0].name, "podkit-files/app-1/1-site_conf");
		assert!(
			lines(container(&units)).contains(
				&"Volume=./podkit-files/app-1/1-site_conf:/etc/nginx/conf.d/site conf:ro"
			)
		);
		assert_eq!(units.secret().count(), 2);
		assert!(units.public().all(|f| !f.content.contains("server {}")));
	}

	#[test]
	fn gpus_become_devices_like_the_agent_maps_them() {
		let mut s = spec();
		s.gpus = Some(crate::GpuRequest {
			vendor: GpuVendor::Nvidia,
			devices: GpuDevices::Count(2),
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(lines(&unit).contains(&"AddDevice=nvidia.com/gpu=0"));
		assert!(lines(&unit).contains(&"AddDevice=nvidia.com/gpu=1"));
		s.gpus = Some(crate::GpuRequest {
			vendor: GpuVendor::Amd,
			devices: GpuDevices::Ids(vec!["renderD128".into()]),
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(lines(&unit).contains(&"AddDevice=/dev/kfd"));
		assert!(lines(&unit).contains(&"AddDevice=/dev/dri/renderD128"));
	}

	#[test]
	fn options_map_to_their_quadlet_keys() {
		let mut s = spec();
		s.options = ContainerOptions {
			restart: Restart::Never,
			user: Some("1000:1000".into()),
			read_only: true,
			init: true,
			tmpfs: vec!["/tmp".into()],
			extra_hosts: vec!["db:10.0.0.2".into()],
			cap_add: vec!["NET_ADMIN".into()],
			cap_drop: vec!["ALL".into()],
			sysctls: vec![("net.core.somaxconn".into(), "1024".into())],
			ulimits: vec![crate::Ulimit {
				name: "nofile".into(),
				soft: 1024,
				hard: 4096,
			}],
			dns: vec!["1.1.1.1".into()],
			group_add: vec!["video".into()],
			..ContainerOptions::default()
		};
		let unit = units_of(&s).containers.remove(0).content;
		let l = lines(&unit);
		for want in [
			"Restart=no",
			"User=1000",
			"Group=1000",
			"ReadOnly=true",
			"RunInit=true",
			"Tmpfs=/tmp",
			"AddHost=db:10.0.0.2",
			"AddCapability=NET_ADMIN",
			"DropCapability=ALL",
			"Sysctl=net.core.somaxconn=1024",
			"Ulimit=nofile=1024:4096",
			"DNS=1.1.1.1",
			"PodmanArgs=--group-add=video",
		] {
			assert!(l.contains(&want), "missing {want:?} in\n{unit}");
		}
	}

	#[test]
	fn depends_on_orders_the_units() {
		let s = spec();
		let after = vec!["stack-4-db".to_owned()];
		let units = generate(
			&s,
			&Options {
				description: "podkit stack shop, service web",
				after: &after,
			},
		);
		let l = lines(container(&units));
		assert!(l.contains(&"Description=podkit stack shop, service web"));
		assert!(l.contains(&"Requires=podkit-stack-4-db.service"));
		assert!(l.contains(&"After=podkit-stack-4-db.service"));
	}

	#[test]
	fn an_mcp_bridge_is_explained_not_dropped() {
		let mut s = spec();
		s.options.mcp_bridge = Some(crate::McpBridge {
			port: 8000,
			path: "/mcp".into(),
			command: vec!["npx".into(), "server".into()],
		});
		let unit = units_of(&s).containers.remove(0).content;
		assert!(
			unit.contains("port 8000 at /mcp through podkit-agent's bridge"),
			"{unit}"
		);
		assert!(unit.contains("# MCP command: npx server"), "{unit}");
	}

	#[test]
	fn the_readme_says_how_to_install_in_what_order_and_what_stops() {
		let a = units_of(&spec());
		let mut db = spec();
		db.service = "db-2".into();
		let b = units_of(&db);
		let text = readme(
			"project shop",
			&[
				ReadmeEntry {
					label: "database main",
					units: &b,
				},
				ReadmeEntry {
					label: "application web",
					units: &a,
				},
			],
			true,
			&["stack old: the compose file no longer parses".to_owned()],
		);
		assert!(text.contains("systemctl --user daemon-reload"), "{text}");
		assert!(text.contains("chmod 600"), "{text}");
		let db_at = text.find("start podkit-db-2.service").unwrap();
		let app_at = text.find("start podkit-app-1.service").unwrap();
		assert!(db_at < app_at, "{text}");
		for word in ["Traefik routing", "Deploys", "Backups", "Cron jobs"] {
			assert!(text.contains(word), "missing {word:?} in\n{text}");
		}
		assert!(text.contains("## Not exported"), "{text}");
		assert!(text.contains("stack old"), "{text}");

		let without = readme("project shop", &[], false, &[]);
		assert!(!without.contains("chmod 600"));
		assert!(without.contains("This export has no env files"));
		assert!(!without.contains("## Not exported"));
	}
}
