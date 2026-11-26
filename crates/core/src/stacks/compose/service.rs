//! One compose service: every key is supported, ignored with a warning, or
//! refused with its path.

use std::collections::{BTreeMap, BTreeSet};

use proto::{Healthcheck, Restart, Ulimit};
use serde_json::{Map, Value};

use super::value::{self, scalar};
use super::{Condition, Ctx, Mount, Service};

/// Capabilities that give a container the host (`cap_add`), upper case.
const DANGEROUS_CAPS: [&str; 5] = ["ALL", "SYS_ADMIN", "SYS_MODULE", "SYS_PTRACE", "SYS_RAWIO"];

/// Keys that only tune behaviour podkit does not offer; they are ignored.
const IGNORED: [&str; 41] = [
	"logging",
	"platform",
	"tty",
	"stdin_open",
	"pull_policy",
	"stop_signal",
	"profiles",
	"links",
	"security_opt",
	"mem_reservation",
	"memswap_limit",
	"mem_swappiness",
	"pids_limit",
	"cpu_shares",
	"cpuset",
	"cpu_period",
	"cpu_quota",
	"cpu_count",
	"cpu_percent",
	"cpu_rt_period",
	"cpu_rt_runtime",
	"oom_kill_disable",
	"oom_score_adj",
	"domainname",
	"mac_address",
	"annotations",
	"develop",
	"post_start",
	"pre_stop",
	"dns_search",
	"dns_opt",
	"storage_opt",
	"exclude_from_hc",
	"blkio_config",
	"device_cgroup_rules",
	"attach",
	"label_file",
	"use_api_socket",
	"cgroup",
	"isolation",
	"credential_spec",
];

/// Keys that would hand the host to the container, or that need a feature
/// podkit does not have, with the reason shown to the user.
const REFUSED: [(&str, &str); 13] = [
	("build", "build is not supported, use an image"),
	("privileged", "privileged containers are not supported"),
	("devices", "device passthrough is not supported"),
	(
		"pid",
		"sharing the host or another container's PID namespace is not supported",
	),
	("ipc", "sharing an IPC namespace is not supported"),
	("uts", "sharing a UTS namespace is not supported"),
	("userns_mode", "user namespace modes are not supported"),
	("cgroup_parent", "cgroup_parent is not supported"),
	("runtime", "custom runtimes are not supported"),
	("extends", "extends is not supported, write the service out"),
	(
		"volumes_from",
		"volumes_from is not supported, share a named volume",
	),
	("external_links", "external_links are not supported"),
	("gpus", "gpus is not supported in compose files yet"),
];

pub(super) fn parse(
	ctx: &mut Ctx<'_>,
	name: &str,
	value: &Value,
	declared: &BTreeSet<String>,
) -> Option<Service> {
	let base = format!("services.{name}");
	let Some(map) = value.as_object() else {
		ctx.issue(Some(name), &base, "a service must be a mapping");
		return None;
	};
	let before = ctx.issues.len();
	let mut service = Service {
		name: name.to_string(),
		image: String::new(),
		command: None,
		entrypoint: None,
		env: BTreeMap::new(),
		expose: BTreeSet::new(),
		ports: Vec::new(),
		mounts: Vec::new(),
		depends_on: BTreeMap::new(),
		memory_bytes: None,
		cpu_cores: None,
		shm_bytes: None,
		stop_timeout_s: None,
		aliases: BTreeSet::new(),
		options: proto::ContainerOptions::default(),
	};
	// `env_file` is applied below `environment`, whatever order they are in.
	let mut file_env: Vec<(String, String)> = Vec::new();
	let mut inline_env: Vec<(String, String)> = Vec::new();
	let mut b = Builder {
		ctx,
		name,
		declared,
	};

	for (key, value) in map {
		let path = format!("{base}.{key}");
		b.key(
			&mut service,
			key,
			value,
			&path,
			&mut file_env,
			&mut inline_env,
		);
	}

	if service.image.is_empty() && !map.contains_key("build") {
		b.ctx
			.issue(Some(name), &format!("{base}.image"), "image is required");
	}
	service.env.extend(file_env);
	service.env.extend(inline_env);
	(b.ctx.issues.len() == before).then_some(service)
}

struct Builder<'a, 'c> {
	ctx: &'a mut Ctx<'c>,
	name: &'a str,
	declared: &'a BTreeSet<String>,
}

impl Builder<'_, '_> {
	fn issue(&mut self, path: &str, message: impl Into<String>) {
		self.ctx.issue(Some(self.name), path, message);
	}

	/// Whether `bytes` more of file content may be mounted. A file is copied into each
	/// mount of it, so one config mounted thousands of times was gigabytes; past the
	/// limit the stack is refused, once, and nothing more is copied.
	fn room(&mut self, path: &str, bytes: usize) -> bool {
		if self.ctx.mounted > super::MAX_MOUNTED {
			return false;
		}
		self.ctx.mounted += bytes;
		if self.ctx.mounted > super::MAX_MOUNTED {
			self.issue(
				path,
				format!(
					"the files mounted into the services come to more than {} MiB",
					super::MAX_MOUNTED >> 20
				),
			);
			return false;
		}
		true
	}

	fn warn(&mut self, path: &str, message: impl Into<String>) {
		self.ctx.warn(Some(self.name), path, message);
	}

	#[allow(clippy::too_many_lines, reason = "one arm per compose key")]
	fn key(
		&mut self,
		s: &mut Service,
		key: &str,
		value: &Value,
		path: &str,
		file_env: &mut Vec<(String, String)>,
		inline_env: &mut Vec<(String, String)>,
	) {
		match key {
			"image" => match scalar(value).map(|v| v.trim().to_string()) {
				Some(image) if !image.is_empty() && !image.contains(char::is_whitespace) => {
					s.image = image;
				}
				_ => self.issue(path, "image must be an image reference"),
			},
			"command" => s.command = self.command(value, path),
			"entrypoint" => s.entrypoint = self.command(value, path),
			"environment" => *inline_env = self.environment(value, path),
			"env_file" => *file_env = self.env_file(value, path),
			"expose" => self.expose(s, value, path),
			"ports" => self.ports(s, value, path),
			"volumes" => self.volumes(s, value, path),
			"tmpfs" => {
				for target in string_list(value) {
					self.tmpfs(s, &target, path);
				}
			}
			"restart" => s.options.restart = self.restart(value, path),
			"depends_on" => s.depends_on = self.depends_on(value, path),
			"healthcheck" => s.options.healthcheck = self.healthcheck(value, path),
			"deploy" => self.deploy(s, value, path),
			"mem_limit" => s.memory_bytes = self.parsed(path, value::bytes(value)),
			"cpus" => s.cpu_cores = self.cpus(value, path),
			"shm_size" => s.shm_bytes = self.parsed(path, value::bytes(value)),
			"stop_grace_period" => {
				s.stop_timeout_s = self
					.parsed(path, value::duration_ms(value))
					.map(|ms| u32::try_from(ms.div_ceil(1000)).unwrap_or(u32::MAX));
			}
			"user" => match scalar(value) {
				Some(user) if !user.is_empty() => s.options.user = Some(user),
				_ => self.issue(path, "user must be a string"),
			},
			"working_dir" => match value.as_str() {
				Some(dir) if dir.starts_with('/') => s.options.working_dir = Some(dir.to_string()),
				_ => self.issue(path, "working_dir must be an absolute path"),
			},
			"hostname" => match value {
				Value::Null => {}
				Value::String(host) if host.is_empty() => {}
				Value::String(host) => {
					s.options.hostname = Some(host.clone());
					s.aliases.insert(host.clone());
				}
				_ => self.issue(path, "hostname must be a string"),
			},
			"container_name" => match value {
				Value::Null => {}
				Value::String(name) if name.is_empty() => {}
				Value::String(name) => {
					s.aliases.insert(name.clone());
				}
				_ => self.issue(path, "container_name must be a string"),
			},
			"read_only" => s.options.read_only = self.boolean(value, path),
			"init" => s.options.init = self.boolean(value, path),
			"extra_hosts" => s.options.extra_hosts = self.extra_hosts(value, path),
			"ulimits" => s.options.ulimits = self.ulimits(value, path),
			"cap_add" => s.options.cap_add = self.caps(value, path, true),
			"cap_drop" => s.options.cap_drop = self.caps(value, path, false),
			"sysctls" => s.options.sysctls = self.sysctls(value, path),
			"dns" => s.options.dns = string_list(value),
			"group_add" => s.options.group_add = string_list(value),
			"labels" => s.options.labels = self.labels(value, path),
			"networks" => self.networks(s, value, path),
			"secrets" | "configs" => self.file_resources(s, key, value, path),
			"network_mode" => match value.as_str() {
				Some("bridge" | "default") => {}
				_ => self.issue(path, "only the default bridge network mode is supported"),
			},
			"scale" => {
				if value::unsigned(value).is_none_or(|n| n > 1) {
					self.issue(path, "running more than one replica is not supported");
				}
			}
			_ if key.starts_with("x-") => {}
			_ => {
				if let Some((_, reason)) = REFUSED.iter().find(|(k, _)| *k == key) {
					self.issue(path, *reason);
				} else if IGNORED.contains(&key) {
					self.warn(path, "ignored");
				} else {
					self.issue(path, "unsupported key");
				}
			}
		}
	}

	fn parsed<T>(&mut self, path: &str, result: Result<T, String>) -> Option<T> {
		match result {
			Ok(v) => Some(v),
			Err(message) => {
				self.issue(path, message);
				None
			}
		}
	}

	fn boolean(&mut self, value: &Value, path: &str) -> bool {
		value::boolean(value).unwrap_or_else(|| {
			self.issue(path, "expected true or false");
			false
		})
	}

	fn cpus(&mut self, value: &Value, path: &str) -> Option<f64> {
		match value::float(value) {
			Some(c) if c > 0.0 => Some(c),
			_ => {
				self.issue(path, "cpus must be a positive number");
				None
			}
		}
	}

	/// `command`/`entrypoint`: a list, or a string split like a shell would.
	fn command(&mut self, value: &Value, path: &str) -> Option<Vec<String>> {
		match value {
			Value::Null => None,
			Value::String(text) => {
				let words = shlex::split(text);
				if words.is_none() {
					self.issue(path, "unbalanced quotes in the command");
				}
				words
			}
			Value::Array(items) => {
				let words: Vec<String> = items.iter().filter_map(scalar).collect();
				if words.len() == items.len() {
					Some(words)
				} else {
					self.issue(path, "every command word must be a string");
					None
				}
			}
			_ => {
				self.issue(path, "expected a string or a list of strings");
				None
			}
		}
	}

	/// A `KEY=value` list or a mapping. A key without a value takes the one
	/// from the stack's variables, or is left out.
	fn environment(&mut self, value: &Value, path: &str) -> Vec<(String, String)> {
		let mut out = Vec::new();
		match value {
			Value::Object(map) => {
				for (key, v) in map {
					match v {
						Value::Null => {
							if let Some(found) = self.ctx.inputs.env.get(key) {
								out.push((key.clone(), found.clone()));
							}
						}
						other => match scalar(other) {
							Some(text) => out.push((key.clone(), text)),
							None => self.issue(
								&format!("{path}.{key}"),
								"an environment value must be a string, number or boolean",
							),
						},
					}
				}
			}
			Value::Array(items) => {
				for item in items {
					let Some(text) = scalar(item) else {
						self.issue(path, "every environment entry must be KEY=value");
						continue;
					};
					match text.split_once('=') {
						Some((key, v)) => out.push((key.to_string(), v.to_string())),
						None => {
							if let Some(found) = self.ctx.inputs.env.get(&text) {
								out.push((text.clone(), found.clone()));
							}
						}
					}
				}
			}
			Value::Null => {}
			_ => self.issue(path, "environment must be a list or a mapping"),
		}
		out
	}

	/// `.env` is the stack's own variables, which are already in the
	/// environment of interpolation; any other file must be one the template
	/// supplied.
	fn env_file(&mut self, value: &Value, path: &str) -> Vec<(String, String)> {
		let entries: Vec<String> = match value {
			Value::String(s) => vec![s.clone()],
			Value::Array(items) => items
				.iter()
				.filter_map(|item| match item {
					Value::Object(map) => map.get("path").and_then(scalar),
					other => scalar(other),
				})
				.collect(),
			_ => {
				self.issue(path, "env_file must be a path or a list of paths");
				return Vec::new();
			}
		};
		let mut out = Vec::new();
		for entry in entries {
			let normalized = normalize_relative(&entry);
			if normalized == ".env" {
				self.warn(
					path,
					"`.env` is the stack's variables; set them on the stack",
				);
				continue;
			}
			match self.ctx.inputs.files.get(&normalized) {
				Some(content) => out.extend(value::dotenv(content)),
				None => self.issue(
					path,
					format!("env_file {entry:?} is not supported; set the variables on the stack"),
				),
			}
		}
		out
	}

	fn expose(&mut self, s: &mut Service, value: &Value, path: &str) {
		let Some(items) = value.as_array() else {
			self.issue(path, "expose must be a list");
			return;
		};
		for item in items {
			let text = scalar(item).unwrap_or_default();
			let number = text.strip_suffix("/tcp").unwrap_or(&text);
			if let Ok(port) = number.parse::<u16>()
				&& port > 0
			{
				s.expose.insert(port);
			} else if text.ends_with("/udp") {
				// Exposing a UDP port tells other services nothing podkit routes.
			} else {
				self.issue(path, format!("invalid exposed port {text:?}"));
			}
		}
	}

	fn ports(&mut self, s: &mut Service, value: &Value, path: &str) {
		let Some(items) = value.as_array() else {
			self.issue(path, "ports must be a list");
			return;
		};
		for (i, item) in items.iter().enumerate() {
			match value::port(item) {
				Ok(parsed) => {
					s.expose.insert(parsed.mapping.container);
					if !parsed.unpublished {
						s.ports.push(parsed.mapping);
					}
				}
				Err(message) => self.issue(&format!("{path}.{i}"), message),
			}
		}
	}

	fn tmpfs(&mut self, s: &mut Service, target: &str, path: &str) {
		let target = target.split(':').next().unwrap_or(target);
		if target.starts_with('/') {
			s.options.tmpfs.push(target.to_string());
		} else {
			self.issue(path, format!("tmpfs path {target:?} must be absolute"));
		}
	}

	fn restart(&mut self, value: &Value, path: &str) -> Restart {
		let text = match value {
			Value::Bool(false) => "no".to_string(),
			other => scalar(other).unwrap_or_default(),
		};
		match text.as_str() {
			"no" | "false" | "" => Restart::Never,
			"always" | "unless-stopped" => Restart::Always,
			t if t == "on-failure" || t.starts_with("on-failure:") => Restart::Always,
			_ => {
				self.issue(path, format!("unknown restart policy {text:?}"));
				Restart::Never
			}
		}
	}

	fn depends_on(&mut self, value: &Value, path: &str) -> BTreeMap<String, Condition> {
		let mut out = BTreeMap::new();
		match value {
			Value::Array(items) => {
				for item in items {
					match scalar(item) {
						Some(name) => {
							out.insert(name, Condition::Started);
						}
						None => self.issue(path, "dependencies are service names"),
					}
				}
			}
			Value::Object(map) => {
				for (name, spec) in map {
					let dep_path = format!("{path}.{name}");
					let condition = spec
						.get("condition")
						.and_then(Value::as_str)
						.unwrap_or("service_started");
					match condition {
						"service_started" => {
							out.insert(name.clone(), Condition::Started);
						}
						"service_healthy" => {
							out.insert(name.clone(), Condition::Healthy);
						}
						"service_completed_successfully" => self.issue(
							&format!("{dep_path}.condition"),
							"one-shot services (service_completed_successfully) are not supported",
						),
						other => self.issue(
							&format!("{dep_path}.condition"),
							format!("unknown condition {other:?}"),
						),
					}
				}
			}
			Value::Null => {}
			_ => self.issue(path, "depends_on must be a list or a mapping"),
		}
		out
	}

	fn healthcheck(&mut self, value: &Value, path: &str) -> Option<Healthcheck> {
		let Some(map) = value.as_object() else {
			self.issue(path, "healthcheck must be a mapping");
			return None;
		};
		let disabled = map.get("disable").and_then(value::boolean) == Some(true);
		let mut check = Healthcheck {
			test: Vec::new(),
			interval_ms: None,
			timeout_ms: None,
			retries: None,
			start_period_ms: None,
		};
		for (key, v) in map {
			let key_path = format!("{path}.{key}");
			match key.as_str() {
				"test" => check.test = self.health_test(v, &key_path),
				"interval" => check.interval_ms = self.parsed(&key_path, value::duration_ms(v)),
				"timeout" => check.timeout_ms = self.parsed(&key_path, value::duration_ms(v)),
				"start_period" => {
					check.start_period_ms = self.parsed(&key_path, value::duration_ms(v));
				}
				"retries" => match value::unsigned(v).and_then(|n| u32::try_from(n).ok()) {
					Some(n) => check.retries = Some(n),
					None => self.issue(&key_path, "retries must be a number"),
				},
				"start_interval" => self.warn(&key_path, "ignored"),
				"disable" => {}
				_ if key.starts_with("x-") => {}
				_ => self.issue(&key_path, "unsupported key"),
			}
		}
		if disabled || check.test == ["NONE"] {
			return Some(Healthcheck {
				test: vec!["NONE".to_string()],
				interval_ms: None,
				timeout_ms: None,
				retries: None,
				start_period_ms: None,
			});
		}
		if check.test.is_empty() {
			self.warn(
				path,
				"a healthcheck without a test keeps the image's own health check",
			);
			return None;
		}
		Some(check)
	}

	fn health_test(&mut self, value: &Value, path: &str) -> Vec<String> {
		match value {
			Value::String(cmd) => vec!["CMD-SHELL".to_string(), cmd.clone()],
			Value::Array(items) => {
				let words: Vec<String> = items.iter().filter_map(scalar).collect();
				match words.first().map(String::as_str) {
					Some("CMD" | "CMD-SHELL") if words.len() > 1 => words,
					Some("NONE") => vec!["NONE".to_string()],
					_ => {
						self.issue(path, "test must start with CMD, CMD-SHELL or NONE");
						Vec::new()
					}
				}
			}
			_ => {
				self.issue(path, "test must be a string or a list");
				Vec::new()
			}
		}
	}

	fn deploy(&mut self, s: &mut Service, value: &Value, path: &str) {
		let Some(map) = value.as_object() else {
			self.issue(path, "deploy must be a mapping");
			return;
		};
		for (key, v) in map {
			let key_path = format!("{path}.{key}");
			match key.as_str() {
				"replicas" => {
					if value::unsigned(v).is_none_or(|n| n > 1) {
						self.issue(&key_path, "running more than one replica is not supported");
					}
				}
				"mode" => {
					if v.as_str() == Some("global") {
						self.issue(&key_path, "global services are not supported");
					}
				}
				"resources" => self.resources(s, v, &key_path),
				"restart_policy" | "placement" | "update_config" | "rollback_config" | "labels"
				| "endpoint_mode" => self.warn(&key_path, "ignored"),
				_ if key.starts_with("x-") => {}
				_ => self.issue(&key_path, "unsupported key"),
			}
		}
	}

	fn resources(&mut self, s: &mut Service, value: &Value, path: &str) {
		let Some(map) = value.as_object() else {
			self.issue(path, "resources must be a mapping");
			return;
		};
		for (key, v) in map {
			let key_path = format!("{path}.{key}");
			match key.as_str() {
				"limits" => {
					let Some(limits) = v.as_object() else {
						self.issue(&key_path, "limits must be a mapping");
						continue;
					};
					for (limit, lv) in limits {
						let limit_path = format!("{key_path}.{limit}");
						match limit.as_str() {
							"memory" => s.memory_bytes = self.parsed(&limit_path, value::bytes(lv)),
							"cpus" => s.cpu_cores = self.cpus(lv, &limit_path),
							"pids" => self.warn(&limit_path, "ignored"),
							_ => self.issue(&limit_path, "unsupported key"),
						}
					}
				}
				"reservations" => {
					let Some(reservations) = v.as_object() else {
						self.issue(&key_path, "reservations must be a mapping");
						continue;
					};
					for (reservation, _) in reservations {
						let reservation_path = format!("{key_path}.{reservation}");
						match reservation.as_str() {
							"memory" | "cpus" => self.warn(&reservation_path, "ignored"),
							"devices" => self.issue(
								&reservation_path,
								"device reservations (GPUs) are not supported in compose files yet",
							),
							_ => self.issue(&reservation_path, "unsupported key"),
						}
					}
				}
				_ => self.issue(&key_path, "unsupported key"),
			}
		}
	}

	fn extra_hosts(&mut self, value: &Value, path: &str) -> Vec<String> {
		match value {
			Value::Array(items) => items
				.iter()
				.filter_map(scalar)
				.map(|entry| entry.replacen('=', ":", 1))
				.collect(),
			Value::Object(map) => map
				.iter()
				.filter_map(|(host, ip)| scalar(ip).map(|ip| format!("{host}:{ip}")))
				.collect(),
			_ => {
				self.issue(path, "extra_hosts must be a list or a mapping");
				Vec::new()
			}
		}
	}

	fn ulimits(&mut self, value: &Value, path: &str) -> Vec<Ulimit> {
		let Some(map) = value.as_object() else {
			self.issue(path, "ulimits must be a mapping");
			return Vec::new();
		};
		let mut out = Vec::new();
		for (name, v) in map {
			#[allow(clippy::cast_possible_truncation, reason = "a limit in whole units")]
			let number = |v: &Value| value::float(v).map(|f| f as i64);
			let limit = match v {
				Value::Object(o) => o
					.get("soft")
					.and_then(number)
					.zip(o.get("hard").and_then(number)),
				other => number(other).map(|n| (n, n)),
			};
			match limit {
				Some((soft, hard)) => out.push(Ulimit {
					name: name.clone(),
					soft,
					hard,
				}),
				None => self.issue(
					&format!("{path}.{name}"),
					"expected a number or {soft, hard}",
				),
			}
		}
		out
	}

	fn caps(&mut self, value: &Value, path: &str, add: bool) -> Vec<String> {
		let caps = string_list(value);
		if add {
			for cap in &caps {
				let upper = cap.to_ascii_uppercase();
				let upper = upper.strip_prefix("CAP_").unwrap_or(&upper);
				if DANGEROUS_CAPS.contains(&upper) {
					self.issue(path, format!("capability {cap} is not allowed"));
				}
			}
		}
		caps
	}

	fn sysctls(&mut self, value: &Value, path: &str) -> Vec<(String, String)> {
		match value {
			Value::Object(map) => map
				.iter()
				.filter_map(|(k, v)| scalar(v).map(|v| (k.clone(), v)))
				.collect(),
			Value::Array(items) => items
				.iter()
				.filter_map(scalar)
				.filter_map(|e| {
					e.split_once('=')
						.map(|(k, v)| (k.to_string(), v.to_string()))
				})
				.collect(),
			_ => {
				self.issue(path, "sysctls must be a list or a mapping");
				Vec::new()
			}
		}
	}

	/// Labels are metadata only. `traefik.*` ones are dropped: routing comes
	/// from the stack's domains.
	fn labels(&mut self, value: &Value, path: &str) -> Vec<(String, String)> {
		let pairs: Vec<(String, String)> = match value {
			Value::Object(map) => map
				.iter()
				.map(|(k, v)| (k.clone(), scalar(v).unwrap_or_default()))
				.collect(),
			Value::Array(items) => items
				.iter()
				.filter_map(scalar)
				.map(|e| match e.split_once('=') {
					Some((k, v)) => (k.to_string(), v.to_string()),
					None => (e, String::new()),
				})
				.collect(),
			_ => {
				self.issue(path, "labels must be a list or a mapping");
				return Vec::new();
			}
		};
		let (dropped, kept): (Vec<_>, Vec<_>) = pairs.into_iter().partition(|(k, _)| {
			k.starts_with("traefik.")
				|| k.starts_with("podkit.")
				|| k.starts_with("com.docker.compose.")
		});
		if !dropped.is_empty() {
			self.warn(
				path,
				"traefik.*, podkit.* and com.docker.compose.* labels are ignored; set the service's domain on the stack",
			);
		}
		kept
	}

	/// Service-level `secrets` and `configs`: the top-level text, mounted as a
	/// file (`/run/secrets/<name>` and `/<name>` unless a target says otherwise).
	fn file_resources(&mut self, s: &mut Service, kind: &str, value: &Value, path: &str) {
		let Some(items) = value.as_array() else {
			self.issue(path, format!("{kind} must be a list"));
			return;
		};
		for (i, item) in items.iter().enumerate() {
			let item_path = format!("{path}.{i}");
			let (source, target) = match item {
				Value::String(name) => (name.clone(), None),
				Value::Object(map) => (
					map.get("source")
						.and_then(Value::as_str)
						.unwrap_or_default()
						.to_string(),
					map.get("target")
						.and_then(Value::as_str)
						.map(str::to_string),
				),
				_ => {
					self.issue(
						&item_path,
						format!("a service {kind} entry is a name or a mapping"),
					);
					continue;
				}
			};
			let key = format!("{kind}.{source}");
			let Some(size) = self.ctx.resources.get(&key).map(String::len) else {
				self.issue(
					&item_path,
					format!("{kind} {source:?} is not defined under the top-level `{kind}`"),
				);
				continue;
			};
			if !self.room(&item_path, size) {
				return;
			}
			let content = self.ctx.resources[&key].clone();
			let dir = if kind == "secrets" {
				"/run/secrets/"
			} else {
				"/"
			};
			let target = match target {
				Some(t) if t.starts_with('/') => t,
				Some(t) => format!("{dir}{t}"),
				None => format!("{dir}{source}"),
			};
			s.mounts.push(Mount::File { target, content });
		}
	}

	/// Networks are collapsed into the stack network; only aliases survive.
	fn networks(&mut self, s: &mut Service, value: &Value, path: &str) {
		match value {
			Value::Array(_) | Value::Null => {}
			Value::Object(map) => {
				for spec in map.values() {
					let aliases = spec.get("aliases").map(string_list).unwrap_or_default();
					s.aliases.extend(aliases);
				}
			}
			_ => self.issue(path, "networks must be a list or a mapping"),
		}
	}

	fn volumes(&mut self, s: &mut Service, value: &Value, path: &str) {
		let Some(items) = value.as_array() else {
			self.issue(path, "volumes must be a list");
			return;
		};
		for (i, item) in items.iter().enumerate() {
			let item_path = format!("{path}.{i}");
			match item {
				Value::String(text) => self.short_volume(s, text, &item_path),
				Value::Object(map) => self.long_volume(s, map, &item_path),
				_ => self.issue(&item_path, "a volume is a string or a mapping"),
			}
		}
	}

	fn short_volume(&mut self, s: &mut Service, text: &str, path: &str) {
		let parts: Vec<&str> = text.split(':').collect();
		let (source, target, mode) = match parts.as_slice() {
			[target] => (None, *target, ""),
			[source, target] => (Some(*source), *target, ""),
			[source, target, mode] => (Some(*source), *target, *mode),
			_ => {
				self.issue(path, format!("invalid volume {text:?}"));
				return;
			}
		};
		let read_only = mode.split(',').any(|m| m == "ro");
		self.mount(s, source, target, read_only, path);
	}

	fn long_volume(&mut self, s: &mut Service, map: &Map<String, Value>, path: &str) {
		let kind = map.get("type").and_then(Value::as_str).unwrap_or("volume");
		let target = map
			.get("target")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let read_only = map
			.get("read_only")
			.and_then(value::boolean)
			.unwrap_or(false);
		if map.get("volume").and_then(|v| v.get("subpath")).is_some() {
			self.issue(path, "volume subpaths are not supported");
			return;
		}
		match kind {
			"tmpfs" => self.tmpfs(s, target, path),
			"volume" | "bind" => {
				let source = map.get("source").and_then(Value::as_str);
				if kind == "bind" && source.is_none() {
					self.issue(path, "a bind mount needs a source");
					return;
				}
				self.mount(s, source, target, read_only, path);
			}
			other => self.issue(path, format!("mount type {other:?} is not supported")),
		}
	}

	/// Resolves one mount: a named volume, an anonymous volume, a template
	/// file, or a relative bind turned into a named volume.
	fn mount(
		&mut self,
		s: &mut Service,
		source: Option<&str>,
		target: &str,
		read_only: bool,
		path: &str,
	) {
		if !target.starts_with('/') || target.contains(':') {
			self.issue(
				path,
				format!("mount target {target:?} must be an absolute path"),
			);
			return;
		}
		let target = target.trim_end_matches('/');
		let target = if target.is_empty() { "/" } else { target };
		let volume = |name: String| Mount::Volume {
			name,
			target: target.to_string(),
			read_only,
		};
		let Some(source) = source.filter(|src| !src.is_empty()) else {
			let name = format!("{}-anon-{}", self.name, sanitize(target));
			s.mounts.push(volume(name));
			return;
		};

		if source.starts_with('/') {
			self.host_path(source, path);
			return;
		}
		if source.starts_with('.') || source.starts_with('~') {
			self.relative_bind(s, source, target, read_only, path);
			return;
		}
		if !source
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
		{
			self.issue(path, format!("invalid volume name {source:?}"));
			return;
		}
		if !self.declared.contains(source) {
			self.warn(
				path,
				format!("volume {source:?} is not declared under `volumes`; it is created anyway"),
			);
		}
		s.mounts.push(volume(source.to_string()));
	}

	fn host_path(&mut self, source: &str, path: &str) {
		if source == "/etc/localtime" || source == "/etc/timezone" {
			self.warn(path, "ignored; set TZ in the environment instead");
		} else if source.ends_with("docker.sock") || source.ends_with("podman.sock") {
			self.issue(
				path,
				"mounting the container engine socket gives the container the host, so it is refused",
			);
		} else {
			self.issue(
				path,
				format!("binding the host path {source:?} is not supported, use a named volume"),
			);
		}
	}

	fn relative_bind(
		&mut self,
		s: &mut Service,
		source: &str,
		target: &str,
		read_only: bool,
		path: &str,
	) {
		let normalized = normalize_relative(source);
		if normalized.starts_with("..") || normalized.starts_with('~') {
			self.issue(path, format!("{source:?} points outside the stack's files"));
			return;
		}
		let files = self.ctx.inputs.files;
		if let Some(content) = files.get(&normalized) {
			if self.room(path, content.len()) {
				s.mounts.push(Mount::File {
					target: target.to_string(),
					content: content.clone(),
				});
			}
			return;
		}
		let prefix = format!("{}/", normalized.trim_end_matches('/'));
		let under = |k: &String| {
			if normalized.is_empty() {
				Some(k.clone())
			} else {
				k.strip_prefix(&prefix).map(str::to_string)
			}
		};
		let size: usize = files
			.iter()
			.filter(|(k, _)| under(k).is_some())
			.map(|(_, v)| v.len())
			.sum();
		if size > 0 && !self.room(path, size) {
			return;
		}
		let nested: Vec<(String, String)> = files
			.iter()
			.filter_map(|(k, v)| under(k).map(|rest| (rest, v.clone())))
			.collect();
		if !nested.is_empty() {
			for (rest, content) in nested {
				s.mounts.push(Mount::File {
					target: format!("{}/{rest}", target.trim_end_matches('/')),
					content,
				});
			}
			return;
		}
		let looks_like_file = normalized
			.rsplit('/')
			.next()
			.is_some_and(|base| base.contains('.') && !base.starts_with('.'));
		if looks_like_file {
			self.issue(
				path,
				format!(
					"{source:?} is a single file bind with no file supplied; there is nothing to mount"
				),
			);
			return;
		}
		let name = format!(
			"bind-{}",
			if normalized.is_empty() {
				"root".to_string()
			} else {
				sanitize(&normalized)
			}
		);
		self.warn(
			path,
			format!("bind mount {source:?} becomes the named volume {name:?}; files shipped next to the compose file are not there"),
		);
		s.mounts.push(Mount::Volume {
			name,
			target: target.to_string(),
			read_only,
		});
	}
}

/// `./a/b/`, `a/b`, `../files/a/b` -> `a/b`: the path relative to the stack's
/// files. Anything else that climbs out keeps its `..`.
pub fn normalize_relative(path: &str) -> String {
	let mut rest = path;
	loop {
		if let Some(r) = rest.strip_prefix("./") {
			rest = r;
		} else if let Some(r) = rest.strip_prefix("../files/") {
			rest = r;
			break;
		} else {
			break;
		}
	}
	if rest == "." || rest == "../files" {
		return String::new();
	}
	rest.trim_end_matches('/').to_string()
}

/// A path as a volume-name fragment.
fn sanitize(path: &str) -> String {
	let cleaned: String = path
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || matches!(c, '_' | '.') {
				c
			} else {
				'-'
			}
		})
		.collect();
	let cleaned = cleaned.trim_matches('-').to_string();
	if cleaned.is_empty() {
		"root".to_string()
	} else {
		cleaned
	}
}

/// A string or a list of scalars, as strings.
fn string_list(value: &Value) -> Vec<String> {
	match value {
		Value::Array(items) => items.iter().filter_map(scalar).collect(),
		other => scalar(other).into_iter().collect(),
	}
}
