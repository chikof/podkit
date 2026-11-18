//! Throwaway: how much of the template ecosystem does a small compose subset cover?
//!
//! `compose-spike <corpus dir>... [--json] [--parser norway|ng|saphyr]`
//! A corpus dir is a checkout of Dokploy/templates (`blueprints/<id>/...`) or of
//! coollabsio/coolify (`templates/compose/<id>.yaml`).

use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- parsers

#[derive(Clone, Copy, PartialEq, Debug)]
enum Parser {
	Norway,
	YamlNg,
	Saphyr,
}

impl Parser {
	fn name(self) -> &'static str {
		match self {
			Parser::Norway => "serde_norway",
			Parser::YamlNg => "serde_yaml_ng",
			Parser::Saphyr => "serde-saphyr",
		}
	}
}

fn has_merge_key(v: &Value) -> bool {
	match v {
		Value::Object(m) => m.contains_key("<<") || m.values().any(has_merge_key),
		Value::Array(a) => a.iter().any(has_merge_key),
		_ => false,
	}
}

/// Parse into plain JSON. `merge` asks the crate to resolve `<<` where it has an API.
fn parse_yaml(p: Parser, text: &str, merge: bool) -> Result<Value, String> {
	match p {
		Parser::Norway => {
			let mut v: serde_norway::Value =
				serde_norway::from_str(text).map_err(|e| e.to_string())?;
			if merge {
				v.apply_merge().map_err(|e| e.to_string())?;
			}
			serde_json::to_value(&v).map_err(|e| e.to_string())
		}
		Parser::YamlNg => {
			let mut v: serde_yaml_ng::Value =
				serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
			if merge {
				v.apply_merge().map_err(|e| e.to_string())?;
			}
			serde_json::to_value(&v).map_err(|e| e.to_string())
		}
		Parser::Saphyr => serde_saphyr::from_str::<Value>(text).map_err(|e| e.to_string()),
	}
}

// ---------------------------------------------------------------- findings

#[derive(Clone, serde::Serialize)]
struct Finding {
	level: &'static str, // refuse | drop | info
	code: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	service: Option<String>,
	detail: String,
}

#[derive(Default)]
struct Analysis {
	findings: Vec<Finding>,
	service_keys: BTreeMap<String, BTreeSet<String>>, // key -> services using it
	top_keys: BTreeSet<String>,
	services: Vec<String>,
	new_fields: BTreeSet<String>,
	unresolved: BTreeSet<String>,
}

impl Analysis {
	fn add(
		&mut self,
		level: &'static str,
		code: &str,
		svc: Option<&str>,
		detail: impl Into<String>,
	) {
		let f = Finding {
			level,
			code: code.to_owned(),
			service: svc.map(str::to_owned),
			detail: detail.into(),
		};
		// one finding per (level, code, service)
		if !self
			.findings
			.iter()
			.any(|g| g.level == f.level && g.code == f.code && g.service == f.service)
		{
			self.findings.push(f);
		}
	}
	fn refuse(&mut self, code: &str, svc: Option<&str>, d: impl Into<String>) {
		self.add("refuse", code, svc, d)
	}
	fn drop_(&mut self, code: &str, svc: Option<&str>, d: impl Into<String>) {
		self.add("drop", code, svc, d)
	}
	fn info(&mut self, code: &str, svc: Option<&str>, d: impl Into<String>) {
		self.add("info", code, svc, d)
	}
}

// ---------------------------------------------------------------- interpolation

struct Env<'a> {
	vars: &'a HashMap<String, String>,
	/// Coolify resolves `SERVICE_*` magic variables itself.
	magic_service: bool,
}

impl Env<'_> {
	fn get(&self, name: &str) -> Option<String> {
		if let Some(v) = self.vars.get(name) {
			return Some(v.clone());
		}
		if self.magic_service && name.starts_with("SERVICE_") {
			return Some("magic".to_owned());
		}
		None
	}
}

fn interpolate(s: &str, env: &Env, unresolved: &mut BTreeSet<String>) -> String {
	let b = s.as_bytes();
	let mut out = String::with_capacity(s.len());
	let mut i = 0;
	while i < b.len() {
		if b[i] == b'$' {
			if i + 1 < b.len() && b[i + 1] == b'$' {
				out.push('$');
				i += 2;
				continue;
			}
			if i + 1 < b.len() && b[i + 1] == b'{' {
				let (mut depth, mut j) = (1, i + 2);
				while j < b.len() && depth > 0 {
					match b[j] {
						b'{' => depth += 1,
						b'}' => depth -= 1,
						_ => {}
					}
					j += 1;
				}
				if depth != 0 {
					out.push_str(&s[i..]);
					break;
				}
				out.push_str(&expand(&s[i + 2..j - 1], env, unresolved));
				i = j;
				continue;
			}
			if i + 1 < b.len() && (b[i + 1].is_ascii_alphabetic() || b[i + 1] == b'_') {
				let mut j = i + 1;
				while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
					j += 1;
				}
				let name = &s[i + 1..j];
				match env.get(name) {
					Some(v) => out.push_str(&v),
					None => {
						unresolved.insert(name.to_owned());
					}
				}
				i = j;
				continue;
			}
		}
		let ch = s[i..].chars().next().unwrap();
		out.push(ch);
		i += ch.len_utf8();
	}
	out
}

fn expand(inner: &str, env: &Env, unresolved: &mut BTreeSet<String>) -> String {
	let name_end = inner
		.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
		.unwrap_or(inner.len());
	let name = &inner[..name_end];
	let rest = &inner[name_end..];
	let val = env.get(name);
	let nonempty = val.as_deref().is_some_and(|v| !v.is_empty());
	let mut resolve = |d: &str| interpolate(d, env, &mut BTreeSet::new());
	if let Some(d) = rest.strip_prefix(":-") {
		return if nonempty { val.unwrap() } else { resolve(d) };
	}
	if let Some(d) = rest.strip_prefix('-') {
		return if val.is_some() {
			val.unwrap()
		} else {
			resolve(d)
		};
	}
	if rest.starts_with(":?") || rest.starts_with('?') {
		if !nonempty {
			unresolved.insert(format!("{name} (required)"));
		}
		return val.unwrap_or_default();
	}
	if let Some(d) = rest.strip_prefix(":+") {
		return if nonempty { resolve(d) } else { String::new() };
	}
	if let Some(d) = rest.strip_prefix('+') {
		return if val.is_some() {
			resolve(d)
		} else {
			String::new()
		};
	}
	match val {
		Some(v) => v,
		None => {
			unresolved.insert(name.to_owned());
			String::new()
		}
	}
}

fn interpolate_value(v: &mut Value, env: &Env, unresolved: &mut BTreeSet<String>) {
	match v {
		Value::String(s) => {
			if s.contains('$') {
				*s = interpolate(s, env, unresolved);
			}
		}
		Value::Array(a) => a
			.iter_mut()
			.for_each(|x| interpolate_value(x, env, unresolved)),
		Value::Object(m) => m
			.values_mut()
			.for_each(|x| interpolate_value(x, env, unresolved)),
		_ => {}
	}
}

// ---------------------------------------------------------------- key tables

enum Class {
	/// Maps onto an existing `DeploySpec` field (or stack-level behaviour that needs none).
	Existing,
	/// Supported, but `DeploySpec` needs a new field (named).
	New(&'static str),
	/// Ignored with a warning.
	Drop,
	/// Handled by dedicated code.
	Special,
	/// Refused.
	Refuse(&'static str),
}

fn classify(key: &str) -> Class {
	use Class::*;
	match key {
		"image" | "command" | "entrypoint" | "environment" | "volumes" | "ports" | "expose"
		| "restart" | "networks" | "container_name" | "hostname" | "mem_limit" | "cpus"
		| "shm_size" | "stop_grace_period" | "gpus" => Existing,
		"deploy" | "depends_on" | "env_file" | "network_mode" | "scale" | "configs" | "secrets" => {
			Special
		}
		"healthcheck" => New("healthcheck"),
		"user" => New("user"),
		"working_dir" => New("working_dir"),
		"cap_add" => New("cap_add"),
		"cap_drop" => New("cap_drop"),
		"labels" => New("labels"),
		"tmpfs" => New("tmpfs"),
		"ulimits" => New("ulimits"),
		"extra_hosts" => New("extra_hosts"),
		"sysctls" => New("sysctls"),
		"read_only" => New("read_only"),
		"init" => New("init"),
		"dns" => New("dns"),
		"group_add" => New("group_add"),
		"logging" | "platform" | "tty" | "stdin_open" | "pull_policy" | "stop_signal"
		| "profiles" | "links" | "mem_reservation" | "memswap_limit" | "mem_swappiness"
		| "oom_kill_disable" | "oom_score_adj" | "cpu_shares" | "cpu_quota" | "cpu_period"
		| "cpuset" | "security_opt" | "domainname" | "mac_address" | "annotations" | "attach"
		| "develop" | "isolation" | "pids_limit" | "storage_opt" | "dns_search" | "dns_opt"
		| "post_start" | "pre_stop" | "cgroup" | "cpu_count" | "cpu_percent"
		| "exclude_from_hc" => Drop,
		"build" => Refuse("build"),
		"privileged" => Refuse("privileged"),
		"devices" | "device_cgroup_rules" => Refuse("devices"),
		"pid" => Refuse("pid"),
		"ipc" => Refuse("ipc"),
		"uts" => Refuse("uts"),
		"userns_mode" => Refuse("userns_mode"),
		"cgroup_parent" => Refuse("cgroup_parent"),
		"extends" => Refuse("extends"),
		"volumes_from" => Refuse("volumes_from"),
		"runtime" => Refuse("runtime"),
		"external_links" => Refuse("external_links"),
		"credential_spec" => Refuse("credential_spec"),
		_ if key.starts_with("x-") => Special,
		_ => Refuse("unknown_key"),
	}
}

const DANGEROUS_CAPS: &[&str] = &["SYS_ADMIN", "ALL", "SYS_MODULE", "SYS_PTRACE", "SYS_RAWIO"];

// ---------------------------------------------------------------- analysis

struct Tpl<'a> {
	/// Normalised file mounts from the template (Dokploy `config.mounts`).
	mounts: &'a [String],
	/// `(service, port)` from the template's domain mappings.
	domains: &'a [(String, Option<u64>)],
	/// Coolify-style: env vars in `SERVICE_FQDN_*`/`SERVICE_URL_*` declare routing.
	dotenv_ok: bool,
}

fn norm_path(p: &str) -> String {
	let mut s = p.trim();
	loop {
		if let Some(r) = s.strip_prefix("../files/") {
			s = r
		} else if let Some(r) = s.strip_prefix("./") {
			s = r
		} else if let Some(r) = s.strip_prefix("../") {
			s = r
		} else {
			break;
		}
	}
	s.trim_start_matches('/').trim_end_matches('/').to_owned()
}

fn mount_match(src: &str, mounts: &[String]) -> bool {
	let n = norm_path(src);
	mounts
		.iter()
		.any(|m| *m == n || m.starts_with(&format!("{n}/")) || n.starts_with(&format!("{m}/")))
}

fn as_u64(v: &Value) -> Option<u64> {
	match v {
		Value::Number(n) => n.as_u64(),
		Value::String(s) => s.trim().parse().ok(),
		_ => None,
	}
}

fn analyze(compose: &Value, tpl: &Tpl) -> Analysis {
	let mut a = Analysis::default();
	let Some(root) = compose.as_object() else {
		a.refuse("not_a_mapping", None, "top level is not a mapping");
		return a;
	};
	for k in root.keys() {
		a.top_keys.insert(k.clone());
		match k.as_str() {
			"services" | "volumes" | "networks" | "configs" | "secrets" => {}
			"version" | "name" => a.info("top_level_ignored", None, k.clone()),
			"include" => a.refuse("include", None, "top-level include"),
			_ if k.starts_with("x-") => a.info("x_extension", None, k.clone()),
			_ => a.refuse("unknown_top_key", None, k.clone()),
		}
	}
	let empty = Map::new();
	let services = root
		.get("services")
		.and_then(Value::as_object)
		.unwrap_or(&empty);
	if services.is_empty() {
		a.refuse("no_services", None, "no services");
		return a;
	}
	a.services = services.keys().cloned().collect();
	let top_vols = root
		.get("volumes")
		.and_then(Value::as_object)
		.unwrap_or(&empty);
	let top_nets = root
		.get("networks")
		.and_then(Value::as_object)
		.unwrap_or(&empty);
	let top_cfgs = root
		.get("configs")
		.and_then(Value::as_object)
		.unwrap_or(&empty);
	let top_secs = root
		.get("secrets")
		.and_then(Value::as_object)
		.unwrap_or(&empty);

	// top-level volumes
	for (n, v) in top_vols {
		if let Some(o) = v.as_object() {
			if o.get("external").is_some_and(|e| e != &Value::Bool(false)) {
				a.refuse("external_volume", None, format!("volume {n} is external"));
			}
			if let Some(d) = o.get("driver").and_then(Value::as_str) {
				if d != "local" {
					a.refuse("volume_driver", None, format!("volume {n} driver {d}"));
				}
			}
			if o.contains_key("driver_opts") {
				a.refuse(
					"volume_driver_opts",
					None,
					format!("volume {n} has driver_opts (bind/nfs/tmpfs)"),
				);
			}
		}
	}
	// top-level networks
	let mut used_nets: BTreeSet<String> = BTreeSet::new();
	for (n, v) in top_nets {
		if let Some(o) = v.as_object() {
			if o.get("internal").is_some_and(|x| x == &Value::Bool(true)) {
				a.drop_(
					"internal_network",
					None,
					format!("network {n} is internal; isolation lost"),
				);
			}
			if o.contains_key("ipam") {
				a.drop_("network_ipam", None, format!("network {n} ipam ignored"));
			}
		}
	}
	// top-level configs/secrets
	for (kind, m) in [("config", top_cfgs), ("secret", top_secs)] {
		for (n, v) in m {
			let o = v.as_object().cloned().unwrap_or_default();
			if o.get("external").is_some_and(|e| e != &Value::Bool(false)) {
				a.refuse(
					"external_config_secret",
					None,
					format!("{kind} {n} is external"),
				);
			} else if o.contains_key("content") || o.contains_key("environment") {
				a.info("config_inline", None, format!("{kind} {n}"));
			} else if let Some(f) = o.get("file").and_then(Value::as_str) {
				if mount_match(f, tpl.mounts) {
					a.info(
						"config_file_mount",
						None,
						format!("{kind} {n} -> template mount"),
					);
				} else {
					a.refuse(
						"config_file_unmapped",
						None,
						format!("{kind} {n} file {f} has no template mount"),
					);
				}
			}
			a.new_fields.insert("file_mounts".into());
		}
	}

	let healthchecked: BTreeSet<&String> = services
		.iter()
		.filter(|(_, s)| {
			s.get("healthcheck")
				.is_some_and(|h| h.get("disable") != Some(&Value::Bool(true)))
		})
		.map(|(n, _)| n)
		.collect();

	for (name, svc) in services {
		let Some(s) = svc.as_object() else {
			a.refuse(
				"service_not_mapping",
				Some(name),
				"service is not a mapping",
			);
			continue;
		};
		let n = Some(name.as_str());
		for (k, v) in s {
			a.service_keys
				.entry(k.clone())
				.or_default()
				.insert(name.clone());
			match classify(k) {
				Class::Existing => {}
				Class::New(f) => {
					a.new_fields.insert(f.into());
				}
				Class::Drop => a.drop_(&format!("drop:{k}"), n, format!("{k} ignored")),
				Class::Refuse("unknown_key") => {
					a.refuse(&format!("unknown_key:{k}"), n, "key not in subset")
				}
				Class::Refuse(c) => a.refuse(c, n, format!("{k} is unsupported")),
				Class::Special => {}
			}
			match k.as_str() {
				"image" => {
					if v.as_str().is_none_or(str::is_empty) {
						a.refuse("no_image", n, "image empty or unresolved");
					}
				}
				"ports" => ports(&mut a, name, v, tpl),
				"volumes" => volumes(&mut a, name, v, top_vols, tpl),
				"environment" => environment(&mut a, name, v),
				"command" | "entrypoint" => {
					if let Some(sv) = v.as_str() {
						if shlex::split(sv).is_none() {
							a.drop_("command_unparsable", n, format!("{k}: cannot shell-split"));
						}
					}
				}
				"env_file" => {
					let files: Vec<&str> = match v {
						Value::String(s) => vec![s],
						Value::Array(x) => x.iter().filter_map(Value::as_str).collect(),
						_ => vec![],
					};
					if tpl.dotenv_ok
						&& !files.is_empty()
						&& files.iter().all(|f| norm_path(f) == ".env")
					{
						a.info(
							"env_file_dotenv",
							n,
							"env_file: .env is the template's own env",
						);
					} else if !files.is_empty() && files.iter().all(|f| mount_match(f, tpl.mounts))
					{
						a.info(
							"env_file_mount",
							n,
							"env_file is a template file mount: parsed at deploy time into env",
						);
					} else {
						a.refuse("env_file", n, format!("env_file {files:?}"));
					}
				}
				"network_mode" => {
					let m = v.as_str().unwrap_or("");
					if m != "bridge" && m != "default" {
						a.refuse("network_mode", n, format!("network_mode: {m}"));
					}
				}
				"scale" => {
					if as_u64(v).is_some_and(|x| x > 1) {
						a.refuse("replicas>1", n, "scale > 1");
					}
				}
				"cap_add" => {
					for c in v.as_array().into_iter().flatten().filter_map(Value::as_str) {
						if DANGEROUS_CAPS.contains(&c.trim_start_matches("CAP_")) {
							a.refuse("dangerous_cap", n, format!("cap_add {c}"));
						}
					}
				}
				"deploy" => deploy(&mut a, name, v),
				"networks" => {
					match v {
						Value::Array(x) => x.iter().filter_map(Value::as_str).for_each(|s| {
							used_nets.insert(s.to_owned());
						}),
						Value::Object(o) => {
							for (nn, cfg) in o {
								used_nets.insert(nn.clone());
								if let Some(c) = cfg.as_object() {
									if c.contains_key("ipv4_address")
										|| c.contains_key("ipv6_address")
									{
										a.drop_("static_ip", n, "static IP ignored");
									}
									if c.contains_key("aliases") {
										a.info("network_aliases", n, "aliases become DNS aliases");
									}
								}
							}
						}
						_ => {}
					}
					a.info(
						"networks_collapsed",
						n,
						"service networks collapsed into the stack network",
					);
				}
				"depends_on" => {
					let deps: Vec<(String, String)> = match v {
						Value::Array(x) => x
							.iter()
							.filter_map(Value::as_str)
							.map(|d| (d.to_owned(), "service_started".to_owned()))
							.collect(),
						Value::Object(o) => o
							.iter()
							.map(|(d, c)| {
								(
									d.clone(),
									c.get("condition")
										.and_then(Value::as_str)
										.unwrap_or("service_started")
										.to_owned(),
								)
							})
							.collect(),
						_ => vec![],
					};
					for (d, cond) in deps {
						if !services.contains_key(&d) {
							a.drop_(
								"depends_on_unknown",
								n,
								format!("depends_on {d}: no such service"),
							);
						}
						match cond.as_str() {
							"service_healthy" => {
								if !healthchecked.contains(&d) {
									a.drop_(
										"healthy_without_healthcheck",
										n,
										format!("{d} has no healthcheck"),
									);
								}
								a.info("depends_on_healthy", n, format!("waits for {d} healthy"));
							}
							"service_completed_successfully" => {
								a.refuse(
									"depends_on_completed",
									n,
									format!("{d} is a run-to-completion init container"),
								);
							}
							_ => {}
						}
					}
				}
				"healthcheck" => {
					if let Some(o) = v.as_object() {
						if o.get("test").is_none() && o.get("disable") != Some(&Value::Bool(true)) {
							a.drop_("healthcheck_no_test", n, "healthcheck without test");
						}
					}
				}
				"labels" => {
					let keys: Vec<String> = match v {
						Value::Object(o) => o.keys().cloned().collect(),
						Value::Array(x) => x
							.iter()
							.filter_map(Value::as_str)
							.map(|s| s.split('=').next().unwrap_or("").to_owned())
							.collect(),
						_ => vec![],
					};
					if keys.iter().any(|k| k.starts_with("traefik.")) {
						a.drop_(
							"traefik_labels",
							n,
							"own Traefik labels ignored; routing comes from the template's domains",
						);
					}
				}
				"secrets" | "configs" => {
					a.new_fields.insert("file_mounts".into());
					for item in v.as_array().into_iter().flatten() {
						let src = item
							.as_str()
							.or_else(|| item.get("source").and_then(Value::as_str))
							.unwrap_or("");
						let known = if k == "secrets" {
							top_secs.contains_key(src)
						} else {
							top_cfgs.contains_key(src)
						};
						if !known {
							a.refuse(
								"config_secret_undeclared",
								n,
								format!("{k} {src} not declared"),
							);
						}
					}
				}
				_ => {}
			}
		}
		if !s.contains_key("image") && !s.contains_key("build") {
			a.refuse("no_image", n, "neither image nor build");
		}
	}
	if used_nets.len() > 1 {
		let internal: BTreeSet<&String> = used_nets.iter().collect();
		a.drop_(
			"multiple_networks",
			None,
			format!(
				"{} networks collapsed: {:?}; isolation between them is lost",
				internal.len(),
				internal
			),
		);
	}
	// depends_on cycles
	let mut deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
	for (n, s) in services {
		let d: Vec<&str> = match s.get("depends_on") {
			Some(Value::Array(x)) => x.iter().filter_map(Value::as_str).collect(),
			Some(Value::Object(o)) => o.keys().map(String::as_str).collect(),
			_ => vec![],
		};
		deps.insert(n, d);
	}
	fn cyc<'a>(
		n: &'a str,
		deps: &BTreeMap<&'a str, Vec<&'a str>>,
		stack: &mut Vec<&'a str>,
	) -> bool {
		if stack.contains(&n) {
			return true;
		}
		stack.push(n);
		let r = deps
			.get(n)
			.into_iter()
			.flatten()
			.any(|d| cyc(d, deps, stack));
		stack.pop();
		r
	}
	if deps.keys().any(|n| cyc(n, &deps, &mut vec![])) {
		a.refuse("depends_on_cycle", None, "depends_on cycle");
	}
	// domain mappings
	for (svc, port) in tpl.domains {
		match services.get(svc) {
			None => a.refuse(
				"domain_service_missing",
				None,
				format!("domain targets {svc}, not in compose"),
			),
			Some(s) => {
				if let Some(p) = port {
					let declared = ports_of(s).contains(p);
					if !declared {
						a.info(
							"domain_port_undeclared",
							Some(svc),
							format!(
								"port {p} not in ports/expose (fine: routing only needs the port)"
							),
						);
					}
				}
			}
		}
	}
	a
}

/// Container ports a service declares via `ports` and `expose`.
fn ports_of(s: &Value) -> BTreeSet<u64> {
	let mut out = BTreeSet::new();
	for e in s
		.get("expose")
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
	{
		if let Some(p) = as_u64(e) {
			out.insert(p);
		}
	}
	for p in s
		.get("ports")
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
	{
		match p {
			Value::Object(o) => {
				if let Some(t) = o.get("target").and_then(as_u64) {
					out.insert(t);
				}
			}
			other => {
				let t = other
					.as_str()
					.map(str::to_owned)
					.or_else(|| other.as_u64().map(|x| x.to_string()))
					.unwrap_or_default();
				let t = t
					.split('/')
					.next()
					.unwrap_or("")
					.rsplit(':')
					.next()
					.unwrap_or("")
					.to_owned();
				if let Ok(x) = t.parse() {
					out.insert(x);
				}
			}
		}
	}
	out
}

fn ports(a: &mut Analysis, svc: &str, v: &Value, tpl: &Tpl) {
	let n = Some(svc);
	for p in v.as_array().into_iter().flatten() {
		let (host, container, proto, ip): (String, String, String, bool) = match p {
			Value::Object(o) => (
				o.get("published")
					.map(|x| {
						x.as_str()
							.map(str::to_owned)
							.unwrap_or_else(|| x.to_string())
					})
					.filter(|s| s != "null")
					.unwrap_or_default(),
				o.get("target")
					.map(|x| {
						x.as_str()
							.map(str::to_owned)
							.unwrap_or_else(|| x.to_string())
					})
					.unwrap_or_default(),
				o.get("protocol")
					.and_then(Value::as_str)
					.unwrap_or("tcp")
					.to_owned(),
				o.contains_key("host_ip"),
			),
			other => {
				let raw = other
					.as_str()
					.map(str::to_owned)
					.unwrap_or_else(|| other.to_string());
				let (body, proto) = raw
					.split_once('/')
					.map(|(b, p)| (b.to_owned(), p.to_owned()))
					.unwrap_or((raw.clone(), "tcp".into()));
				let parts: Vec<&str> = body.split(':').collect();
				match parts.len() {
					1 => (String::new(), parts[0].to_owned(), proto, false),
					2 => (parts[0].to_owned(), parts[1].to_owned(), proto, false),
					3 => (parts[1].to_owned(), parts[2].to_owned(), proto, true),
					_ => {
						a.refuse("bad_port", n, format!("cannot parse port {raw}"));
						continue;
					}
				}
			}
		};
		if host.contains("stub") || container.contains("stub") {
			a.info(
				"port_from_variable",
				n,
				format!("{host}:{container}: depends on a template variable"),
			);
			continue;
		}
		if host.contains('-') || container.contains('-') {
			a.refuse("port_range", n, format!("port range {host}:{container}"));
			continue;
		}
		let Ok(cport) = container.parse::<u64>() else {
			a.refuse("bad_port", n, format!("container port {container:?}"));
			continue;
		};
		if proto != "tcp" {
			a.new_fields.insert("published_port.protocol".into());
			a.info("udp_port", n, format!("{cport}/{proto}"));
		}
		if ip {
			a.new_fields.insert("published_port.host_ip".into());
		}
		let routed = tpl
			.domains
			.iter()
			.any(|(s, p)| s == svc && *p == Some(cport));
		if routed {
			a.info(
				"port_to_routing",
				n,
				format!("{cport} is routed through Traefik, not published"),
			);
		} else if host.is_empty() {
			a.info(
				"port_internal",
				n,
				format!("{cport} stays on the stack network"),
			);
		} else if let Ok(h) = host.parse::<u64>() {
			if h < 1024 {
				a.info(
					"privileged_host_port",
					n,
					format!("host port {h} < 1024: rootless cannot bind without sysctl"),
				);
			}
			a.info("published_port", n, format!("{h}:{cport}"));
		} else {
			a.refuse("bad_port", n, format!("host port {host:?}"));
		}
	}
}

fn environment(a: &mut Analysis, svc: &str, v: &Value) {
	match v {
		Value::Array(x) => {
			for e in x {
				if e.as_str().is_none() {
					a.drop_("environment_odd_item", Some(svc), format!("{e}"));
				}
			}
		}
		Value::Object(_) => {}
		_ => a.refuse(
			"environment_shape",
			Some(svc),
			"environment is neither list nor map",
		),
	}
}

fn volumes(a: &mut Analysis, svc: &str, v: &Value, top_vols: &Map<String, Value>, tpl: &Tpl) {
	let n = Some(svc);
	for item in v.as_array().into_iter().flatten() {
		let (kind, src, dst, has_content) = match item {
			Value::Object(o) => (
				o.get("type")
					.and_then(Value::as_str)
					.unwrap_or("volume")
					.to_owned(),
				o.get("source")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_owned(),
				o.get("target")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_owned(),
				o.contains_key("content"),
			),
			other => {
				let raw = other.as_str().unwrap_or("").to_owned();
				let parts: Vec<&str> = raw.split(':').collect();
				match parts.as_slice() {
					[d] => ("volume".into(), String::new(), (*d).to_owned(), false),
					[s, d] | [s, d, _] => {
						let kind = if s.starts_with('/') || s.starts_with('.') || s.starts_with('~')
						{
							"bind"
						} else {
							"volume"
						};
						(kind.into(), (*s).to_owned(), (*d).to_owned(), false)
					}
					_ => {
						a.refuse("bad_volume", n, format!("cannot parse volume {raw:?}"));
						continue;
					}
				}
			}
		};
		if src.contains("docker.sock") || src.contains("podman.sock") {
			a.refuse("docker_sock", n, format!("mounts {src}"));
			continue;
		}
		match kind.as_str() {
			"tmpfs" => {
				a.new_fields.insert("tmpfs".into());
			}
			"volume" => {
				if src.is_empty() {
					a.info("anonymous_volume", n, dst);
				} else if !top_vols.contains_key(&src) {
					a.info(
						"volume_undeclared",
						n,
						format!("{src} not in top-level volumes (compose would reject)"),
					);
				}
			}
			"bind" => {
				if has_content {
					a.info("bind_with_content", n, format!("{dst} from inline content"));
				} else if src.starts_with('/') || src.starts_with('~') {
					if matches!(src.as_str(), "/etc/localtime" | "/etc/timezone") {
						a.drop_("localtime_bind", n, format!("{src} ignored; set TZ"));
					} else {
						a.refuse("host_bind", n, format!("host path {src}"));
					}
				} else if mount_match(&src, tpl.mounts) {
					a.info("file_mount", n, format!("{src} -> template mount"));
					a.new_fields.insert("file_mounts".into());
				} else {
					let base = src.rsplit('/').next().unwrap_or("");
					if base.contains('.') && !base.starts_with('.') {
						a.refuse(
							"bind_file_unmapped",
							n,
							format!("{src} looks like a file and has no template mount"),
						);
					} else {
						a.drop_(
							"bind_to_named_volume",
							n,
							format!("{src} mapped to a named volume"),
						);
					}
				}
			}
			other => a.refuse("volume_type", n, format!("type {other}")),
		}
	}
}

fn deploy(a: &mut Analysis, svc: &str, v: &Value) {
	let n = Some(svc);
	let Some(o) = v.as_object() else { return };
	for (k, x) in o {
		match k.as_str() {
			"replicas" => {
				if as_u64(x).is_some_and(|r| r > 1) {
					a.refuse("replicas>1", n, "deploy.replicas > 1");
				}
			}
			"mode" => {
				if x.as_str() == Some("global") {
					a.refuse("deploy_mode_global", n, "global mode");
				}
			}
			"resources" => {
				if x.get("limits").is_some() {
					a.info(
						"resource_limits",
						n,
						"deploy.resources.limits -> memory_bytes/cpu_cores",
					);
				}
				if x.pointer("/reservations/devices").is_some() {
					a.info("gpu_reservation", n, "reservations.devices -> GpuRequest");
				}
			}
			_ => a.drop_(
				&format!("drop:deploy.{k}"),
				n,
				format!("deploy.{k} ignored"),
			),
		}
	}
}

// ---------------------------------------------------------------- corpora

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Corpus {
	Dokploy,
	Coolify,
}

struct Item {
	corpus: Corpus,
	id: String,
	compose: PathBuf,
	toml: Option<PathBuf>,
}

fn discover(dir: &Path) -> Vec<Item> {
	let mut out = vec![];
	if let Ok(rd) = fs::read_dir(dir.join("blueprints")) {
		for e in rd.flatten() {
			let p = e.path();
			let c = p.join("docker-compose.yml");
			if c.is_file() {
				let t = p.join("template.toml");
				out.push(Item {
					corpus: Corpus::Dokploy,
					id: e.file_name().to_string_lossy().into_owned(),
					compose: c,
					toml: t.is_file().then_some(t),
				});
			}
		}
	}
	if let Ok(rd) = fs::read_dir(dir.join("templates/compose")) {
		for e in rd.flatten() {
			let p = e.path();
			if p.extension().is_some_and(|x| x == "yaml" || x == "yml") {
				let id = p.file_stem().unwrap().to_string_lossy().into_owned();
				out.push(Item {
					corpus: Corpus::Coolify,
					id,
					compose: p,
					toml: None,
				});
			}
		}
	}
	out.sort_by(|a, b| (a.corpus, &a.id).cmp(&(b.corpus, &b.id)));
	out
}

/// `${helper:arg}` / `${var}` occurrences in a template string: (name, has_arg).
fn refs(s: &str) -> Vec<(String, bool)> {
	let mut out = vec![];
	let mut rest = s;
	while let Some(i) = rest.find("${") {
		let Some(j) = rest[i..].find('}') else { break };
		let inner = &rest[i + 2..i + j];
		let name = inner.split(':').next().unwrap_or("");
		if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
			out.push((name.to_owned(), inner.contains(':')));
		}
		rest = &rest[i + j + 1..];
	}
	out
}

fn stub(s: &str) -> String {
	let mut out = String::new();
	let mut rest = s;
	while let Some(i) = rest.find("${") {
		out.push_str(&rest[..i]);
		let Some(j) = rest[i..].find('}') else {
			out.push_str(&rest[i..]);
			rest = "";
			break;
		};
		let name = rest[i + 2..i + j].split(':').next().unwrap_or("");
		out.push_str(match name {
			"randomPort" => "20000",
			"domain" => "app.example.test",
			_ => "stub",
		});
		rest = &rest[i + j + 1..];
	}
	out.push_str(rest);
	out
}

const KNOWN_HELPERS: &[&str] = &[
	"domain",
	"password",
	"base64",
	"hash",
	"uuid",
	"randomPort",
	"email",
	"timestamp",
	"timestampms",
	"timestamps",
	"jwt",
	"username",
];

#[derive(Default, serde::Serialize)]
struct TomlInfo {
	parsed: bool,
	variables: Vec<String>,
	helpers: BTreeMap<String, usize>,
	/// Names referenced as `${x}` that are neither helpers nor declared variables.
	undefined_refs: Vec<String>,
	env_entries: usize,
	domains: usize,
	mounts: usize,
	domain_has_path: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	error: Option<String>,
}

struct Loaded {
	info: TomlInfo,
	env: HashMap<String, String>,
	mounts: Vec<String>,
	domains: Vec<(String, Option<u64>)>,
}

fn load_toml(path: Option<&Path>) -> Loaded {
	let mut l = Loaded {
		info: TomlInfo::default(),
		env: HashMap::new(),
		mounts: vec![],
		domains: vec![],
	};
	let Some(path) = path else { return l };
	let text = fs::read_to_string(path).unwrap_or_default();
	let v: Value = match toml::from_str::<toml::Value>(&text)
		.map_err(|e| e.to_string())
		.and_then(|t| serde_json::to_value(t).map_err(|e| e.to_string()))
	{
		Ok(v) => v,
		Err(e) => {
			l.info.error = Some(e);
			return l;
		}
	};
	l.info.parsed = true;
	let vars: BTreeSet<String> = v
		.get("variables")
		.and_then(Value::as_object)
		.map(|o| o.keys().cloned().collect())
		.unwrap_or_default();
	l.info.variables = vars.iter().cloned().collect();
	// helper census over the whole file
	let mut undefined = BTreeSet::new();
	for (name, _) in refs(&text) {
		if KNOWN_HELPERS.contains(&name.as_str()) {
			*l.info.helpers.entry(name).or_default() += 1;
		} else if !vars.contains(&name) {
			undefined.insert(name);
		}
	}
	l.info.undefined_refs = undefined.into_iter().collect();
	if let Some(cfg) = v.get("config") {
		match cfg.get("env") {
			Some(Value::Array(x)) => {
				for e in x.iter().filter_map(Value::as_str) {
					if let Some((k, val)) = e.split_once('=') {
						l.env.insert(k.trim().to_owned(), stub(val));
					}
				}
			}
			Some(Value::Object(o)) => {
				for (k, val) in o {
					l.env.insert(
						k.clone(),
						stub(
							&val.as_str()
								.map(str::to_owned)
								.unwrap_or_else(|| val.to_string()),
						),
					);
				}
			}
			_ => {}
		}
		l.info.env_entries = l.env.len();
		for m in cfg
			.get("mounts")
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
		{
			if let Some(p) = m.get("filePath").and_then(Value::as_str) {
				l.mounts.push(norm_path(p));
			}
		}
		l.info.mounts = l.mounts.len();
		for d in cfg
			.get("domains")
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
		{
			if let Some(s) = d.get("serviceName").and_then(Value::as_str) {
				l.domains
					.push((s.to_owned(), d.get("port").and_then(as_u64)));
			}
			if d.get("path").is_some() {
				l.info.domain_has_path = true;
			}
		}
		l.info.domains = l.domains.len();
	}
	l
}

#[derive(serde::Serialize)]
struct Entry {
	id: String,
	corpus: Corpus,
	tier: &'static str, // full | after_drop | refused
	services: Vec<String>,
	service_keys: Vec<String>,
	top_level_keys: Vec<String>,
	refusals: Vec<String>,
	drops: Vec<String>,
	new_agent_fields: Vec<String>,
	unresolved_vars: Vec<String>,
	findings: Vec<Finding>,
	template_toml: Option<TomlInfo>,
	yaml: YamlInfo,
}

#[derive(Default, serde::Serialize)]
struct YamlInfo {
	parsed: bool,
	merge_keys: bool,
	anchors: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	error: Option<String>,
}

fn build_entry(it: &Item, parser: Parser) -> Entry {
	let text = fs::read_to_string(&it.compose).unwrap_or_default();
	let loaded = load_toml(it.toml.as_deref());
	let yaml = YamlInfo {
		parsed: false,
		merge_keys: text.lines().any(|l| l.contains("<<:")),
		anchors: text
			.lines()
			.any(|l| l.contains(": &") || l.trim_start().starts_with("- &")),
		error: None,
	};
	let mut e = Entry {
		id: it.id.clone(),
		corpus: it.corpus,
		tier: "refused",
		services: vec![],
		service_keys: vec![],
		top_level_keys: vec![],
		refusals: vec![],
		drops: vec![],
		new_agent_fields: vec![],
		unresolved_vars: vec![],
		findings: vec![],
		template_toml: (it.corpus == Corpus::Dokploy).then_some(loaded.info),
		yaml,
	};
	if let Some(t) = &e.template_toml {
		if !t.parsed {
			e.findings.push(Finding {
				level: "refuse",
				code: "template_toml".into(),
				service: None,
				detail: t
					.error
					.clone()
					.unwrap_or_else(|| "missing template.toml".into()),
			});
		}
	}
	let mut v = match parse_yaml(parser, &text, true) {
		Ok(v) => v,
		Err(err) => {
			e.yaml.error = Some(err.clone());
			e.findings.push(Finding {
				level: "refuse",
				code: "yaml_parse".into(),
				service: None,
				detail: err,
			});
			finish(&mut e);
			return e;
		}
	};
	e.yaml.parsed = true;
	let env = Env {
		vars: &loaded.env,
		magic_service: it.corpus == Corpus::Coolify,
	};
	let mut unresolved = BTreeSet::new();
	interpolate_value(&mut v, &env, &mut unresolved);
	let tpl = Tpl {
		mounts: &loaded.mounts,
		domains: &loaded.domains,
		dotenv_ok: it.corpus == Corpus::Dokploy,
	};
	let a = analyze(&v, &tpl);
	e.services = a.services;
	e.service_keys = a.service_keys.keys().cloned().collect();
	e.top_level_keys = a.top_keys.into_iter().collect();
	e.new_agent_fields = a.new_fields.into_iter().collect();
	e.unresolved_vars = unresolved.into_iter().collect();
	e.findings.extend(a.findings);
	finish(&mut e);
	e
}

fn finish(e: &mut Entry) {
	let set = |lvl: &str, f: &[Finding]| -> Vec<String> {
		f.iter()
			.filter(|x| x.level == lvl)
			.map(|x| x.code.clone())
			.collect::<BTreeSet<_>>()
			.into_iter()
			.collect()
	};
	e.refusals = set("refuse", &e.findings);
	e.drops = set("drop", &e.findings);
	e.tier = if !e.refusals.is_empty() {
		"refused"
	} else if !e.drops.is_empty() {
		"after_drop"
	} else {
		"full"
	};
}

// ---------------------------------------------------------------- summary

fn count_by<'a>(it: impl Iterator<Item = &'a String>) -> Vec<(String, usize)> {
	let mut m: BTreeMap<&String, usize> = BTreeMap::new();
	for k in it {
		*m.entry(k).or_default() += 1;
	}
	let mut v: Vec<(String, usize)> = m.into_iter().map(|(k, c)| (k.clone(), c)).collect();
	v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
	v
}

fn pairs(v: Vec<(String, usize)>) -> Value {
	Value::Array(
		v.into_iter()
			.map(|(k, c)| json!({"reason": k, "templates": c}))
			.collect(),
	)
}

fn summarize(entries: &[&Entry]) -> Value {
	let total = entries.len();
	let n = |t: &str| entries.iter().filter(|e| e.tier == t).count();
	let (full, drop, refused) = (n("full"), n("after_drop"), n("refused"));
	let pct = |x: usize| {
		if total == 0 {
			0.0
		} else {
			(x as f64 / total as f64 * 1000.0).round() / 10.0
		}
	};
	// "only reason" = templates that a single refusal code blocks
	let sole: Vec<String> = entries
		.iter()
		.filter(|e| e.refusals.len() == 1)
		.map(|e| e.refusals[0].clone())
		.collect();
	// refusal reasons grouped by the key family (drop ":key" suffix)
	let mut svc_key_use: BTreeMap<&String, usize> = BTreeMap::new();
	for e in entries {
		for k in &e.service_keys {
			*svc_key_use.entry(k).or_default() += 1;
		}
	}
	let mut keys: Vec<_> = svc_key_use.into_iter().collect();
	keys.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
	json!({
		"total": total,
		"fully_supported": full,
		"supported_after_dropping": drop,
		"supported": full + drop,
		"refused": refused,
		"supported_pct": pct(full + drop),
		"fully_supported_pct": pct(full),
		"refusal_reasons": pairs(count_by(entries.iter().flat_map(|e| e.refusals.iter()))),
		"sole_refusal_reasons": pairs(count_by(sole.iter())),
		"drop_reasons": pairs(count_by(entries.iter().flat_map(|e| e.drops.iter()))),
		"new_agent_fields": pairs(count_by(entries.iter().filter(|e| e.tier != "refused").flat_map(|e| e.new_agent_fields.iter()))),
		"service_key_usage": keys.into_iter().map(|(k, c)| json!({"key": k, "templates": c})).collect::<Vec<_>>(),
		"unresolved_vars_templates": entries.iter().filter(|e| !e.unresolved_vars.is_empty()).count(),
	})
}

fn parser_report(items: &[Item]) -> Value {
	let mut out = Map::new();
	let texts: Vec<String> = items
		.iter()
		.map(|i| fs::read_to_string(&i.compose).unwrap_or_default())
		.collect();
	let with_merge = texts
		.iter()
		.filter(|t| t.lines().any(|l| l.contains("<<:")))
		.count();
	for p in [Parser::Norway, Parser::YamlNg, Parser::Saphyr] {
		let (mut ok, mut merge_native, mut merge_api) = (0, 0, 0);
		let mut errs: Vec<String> = vec![];
		for (it, t) in items.iter().zip(&texts) {
			match parse_yaml(p, t, false) {
				Ok(v) => {
					ok += 1;
					if t.lines().any(|l| l.contains("<<:")) && !has_merge_key(&v) {
						merge_native += 1;
					}
				}
				Err(e) => errs.push(format!(
					"{}/{}: {}",
					it.corpus_str(),
					it.id,
					e.lines().next().unwrap_or("")
				)),
			}
			if let Ok(v) = parse_yaml(p, t, true) {
				if t.lines().any(|l| l.contains("<<:")) && !has_merge_key(&v) {
					merge_api += 1;
				}
			}
		}
		out.insert(
			p.name().into(),
			json!({"parsed": ok, "failed": items.len() - ok, "merge_resolved_without_api": merge_native, "merge_resolved_with_apply_merge": merge_api, "first_errors": errs.into_iter().take(5).collect::<Vec<_>>()}),
		);
	}
	// typed crate
	let (mut ok, mut errs) = (0, vec![]);
	for (it, t) in items.iter().zip(&texts) {
		match serde_yaml_ng::from_str::<docker_compose_types::Compose>(t) {
			Ok(_) => ok += 1,
			Err(e) => errs.push(format!(
				"{}/{}: {}",
				it.corpus_str(),
				it.id,
				e.to_string().lines().next().unwrap_or("")
			)),
		}
	}
	let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
	for e in &errs {
		let msg = e.split(": ").skip(1).collect::<Vec<_>>().join(": ");
		let key: String = msg.chars().take(60).collect();
		*kinds.entry(key).or_default() += 1;
	}
	let mut kinds: Vec<_> = kinds.into_iter().collect();
	kinds.sort_by(|a, b| b.1.cmp(&a.1));
	out.insert(
		"docker-compose-types (typed, via serde_yaml_ng)".into(),
		json!({"parsed": ok, "failed": items.len() - ok, "top_errors": kinds.into_iter().take(6).map(|(k, c)| json!({"error": k, "files": c})).collect::<Vec<_>>(), "first_errors": errs.into_iter().take(3).collect::<Vec<_>>()}),
	);
	json!({"files": items.len(), "files_with_merge_keys": with_merge, "parsers": out})
}

impl Item {
	fn corpus_str(&self) -> &'static str {
		match self.corpus {
			Corpus::Dokploy => "dokploy",
			Corpus::Coolify => "coolify",
		}
	}
}

fn main() {
	let mut dirs = vec![];
	let (mut json_out, mut parser, mut compare) = (false, Parser::Saphyr, false);
	let mut args = std::env::args().skip(1);
	while let Some(a) = args.next() {
		match a.as_str() {
			"--json" => json_out = true,
			"--probe" => {
				// parse one file with the chosen parser; used for the alias-bomb test
				let path = args.next().expect("--probe <file>");
				let text = fs::read_to_string(&path).unwrap();
				let t = std::time::Instant::now();
				match parse_yaml(parser, &text, true) {
					Ok(v) => println!(
						"{}: ok, {} bytes of json in {:?}",
						parser.name(),
						v.to_string().len(),
						t.elapsed()
					),
					Err(e) => println!(
						"{}: error in {:?}: {}",
						parser.name(),
						t.elapsed(),
						e.lines().next().unwrap_or("")
					),
				}
				return;
			}
			"--compare-parsers" => compare = true,
			"--parser" => {
				parser = match args.next().as_deref() {
					Some("norway") => Parser::Norway,
					Some("ng") => Parser::YamlNg,
					Some("saphyr") => Parser::Saphyr,
					o => panic!("unknown parser {o:?}"),
				}
			}
			_ => dirs.push(PathBuf::from(a)),
		}
	}
	if dirs.is_empty() {
		eprintln!(
			"usage: compose-spike <corpus dir>... [--json] [--parser norway|ng|saphyr] [--compare-parsers]"
		);
		std::process::exit(2);
	}
	let items: Vec<Item> = dirs.iter().flat_map(|d| discover(d)).collect();
	if compare {
		println!(
			"{}",
			serde_json::to_string_pretty(&parser_report(&items)).unwrap()
		);
		return;
	}
	let entries: Vec<Entry> = items.iter().map(|i| build_entry(i, parser)).collect();
	let all: Vec<&Entry> = entries.iter().collect();
	let mut by_corpus = Map::new();
	for c in [Corpus::Dokploy, Corpus::Coolify] {
		let sub: Vec<&Entry> = entries.iter().filter(|e| e.corpus == c).collect();
		if !sub.is_empty() {
			by_corpus.insert(
				if c == Corpus::Dokploy {
					"dokploy"
				} else {
					"coolify"
				}
				.into(),
				summarize(&sub),
			);
		}
	}
	// template.toml census (Dokploy only)
	let mut helpers: BTreeMap<String, usize> = BTreeMap::new();
	let mut undefined: BTreeMap<String, usize> = BTreeMap::new();
	let (mut with_domains, mut with_mounts, mut multi_domain, mut parse_fail) = (0, 0, 0, 0);
	for e in entries.iter().filter(|e| e.corpus == Corpus::Dokploy) {
		if let Some(t) = &e.template_toml {
			if !t.parsed {
				parse_fail += 1;
			}
			for (h, c) in &t.helpers {
				*helpers.entry(h.clone()).or_default() += c;
			}
			for u in &t.undefined_refs {
				*undefined.entry(u.clone()).or_default() += 1;
			}
			with_domains += (t.domains > 0) as usize;
			multi_domain += (t.domains > 1) as usize;
			with_mounts += (t.mounts > 0) as usize;
		}
	}
	let mut undef: Vec<_> = undefined.into_iter().collect();
	undef.sort_by(|a, b| b.1.cmp(&a.1));
	let doc = json!({
		"parser": parser.name(),
		"summary": summarize(&all),
		"by_corpus": by_corpus,
		"template_toml": {"parse_failures": parse_fail, "with_domains": with_domains, "with_multiple_domains": multi_domain, "with_mounts": with_mounts, "helper_uses": helpers, "undefined_ref_names_top": undef.into_iter().take(15).collect::<Vec<_>>()},
		"templates": entries,
	});
	if json_out {
		println!("{}", serde_json::to_string_pretty(&doc).unwrap());
		return;
	}
	let s = &doc["summary"];
	println!(
		"{} templates: {} full, {} after dropping, {} refused",
		s["total"], s["fully_supported"], s["supported_after_dropping"], s["refused"]
	);
	for r in s["refusal_reasons"].as_array().unwrap().iter().take(15) {
		println!(
			"  refused {:>4}  {}",
			r["templates"],
			r["reason"].as_str().unwrap()
		);
	}
	for (k, c) in s["drop_reasons"]
		.as_array()
		.unwrap()
		.iter()
		.map(|r| (r["reason"].as_str().unwrap(), &r["templates"]))
		.take(10)
	{
		println!("  dropped {c:>4}  {k}");
	}
}
