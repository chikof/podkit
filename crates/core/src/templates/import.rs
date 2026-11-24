//! `server templates-import`: converts Dokploy's blueprints (MIT) into
//! podkit templates, keeping only those the compose translation accepts,
//! vendors the logo registry, and writes `IMPORT_REPORT.md` and `NOTICE`.
//!
//! Run through `scripts/templates-import`, which fetches the three source
//! repositories at pinned revisions. Nothing here runs when the server runs.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use super::registry::{Loaded, load};
use super::{Config, DomainMap, Mount, Requires, Template, Variable, logo_type, valid_id};

const USAGE: &str = "usage: server templates-import --dokploy <blueprints dir> --dokploy-rev <rev> \
	--dashboard-icons <svg dir> --dashboard-icons-rev <rev> --simple-icons <icons dir> \
	--simple-icons-rev <rev> [--out crates/core/templates] [--static crates/core/static/logos] \
	[--logos scripts/logos.txt] [--notice NOTICE] [--verify-images[=all]]";

/// Dokploy's licence text, reproduced in `NOTICE`.
const DOKPLOY_COPYRIGHT: &str = "Copyright (c) 2024 Dokploy and Carlos Ortiz";

const MIT: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.";

struct Opts {
	dokploy: PathBuf,
	dokploy_rev: String,
	di: PathBuf,
	di_rev: String,
	si: PathBuf,
	si_rev: String,
	out: PathBuf,
	statics: PathBuf,
	logos: PathBuf,
	notice: PathBuf,
	verify: Verify,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verify {
	No,
	/// Hand-written templates only.
	Own,
	/// Every template; slow, and Docker Hub rate-limits anonymous reads.
	All,
}

/// Runs the importer when the first argument says so; the exit code.
#[must_use]
pub fn cli() -> Option<i32> {
	let args: Vec<String> = std::env::args().collect();
	if args.get(1).map(String::as_str) != Some("templates-import") {
		return None;
	}
	Some(match parse_args(&args[2..]).and_then(|o| run(&o)) {
		Ok(()) => 0,
		Err(e) => {
			eprintln!("templates-import: {e}");
			1
		}
	})
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
	let mut map: BTreeMap<String, String> = BTreeMap::new();
	let mut verify = Verify::Own;
	let mut it = args.iter();
	while let Some(arg) = it.next() {
		match arg.as_str() {
			"--verify-images" => verify = Verify::Own,
			"--verify-images=all" => verify = Verify::All,
			"--no-verify-images" => verify = Verify::No,
			flag if flag.starts_with("--") => {
				let value = it
					.next()
					.ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?;
				map.insert(flag[2..].to_string(), value.clone());
			}
			other => return Err(format!("unexpected {other:?}\n{USAGE}")),
		}
	}
	let need = |k: &str| {
		map.get(k)
			.cloned()
			.ok_or_else(|| format!("--{k} is required\n{USAGE}"))
	};
	let opt = |k: &str, d: &str| map.get(k).cloned().unwrap_or_else(|| d.to_string());
	Ok(Opts {
		dokploy: need("dokploy")?.into(),
		dokploy_rev: need("dokploy-rev")?,
		di: need("dashboard-icons")?.into(),
		di_rev: need("dashboard-icons-rev")?,
		si: need("simple-icons")?.into(),
		si_rev: need("simple-icons-rev")?,
		out: opt("out", "crates/core/templates").into(),
		statics: opt("static", "crates/core/static/logos").into(),
		logos: opt("logos", "scripts/logos.txt").into(),
		notice: opt("notice", "NOTICE").into(),
		verify,
	})
}

fn io<T>(r: std::io::Result<T>, what: &str) -> Result<T, String> {
	r.map_err(|e| format!("{what}: {e}"))
}

// ---------------------------------------------------------------- logos

/// A logo file placed in the registry, and where it came from.
#[derive(Debug, Clone)]
struct Vendored {
	name: String,
	source: &'static str,
	upstream: String,
}

struct Logos<'a> {
	opts: &'a Opts,
	vendored: BTreeMap<String, Vendored>,
}

impl<'a> Logos<'a> {
	fn new(opts: &'a Opts) -> Self {
		Self {
			opts,
			vendored: BTreeMap::new(),
		}
	}

	fn source_path(&self, source: &str, upstream: &str) -> PathBuf {
		match source {
			"di" => self.opts.di.join(format!("{upstream}.svg")),
			_ => self.opts.si.join(format!("{upstream}.svg")),
		}
	}

	/// Copies `<source>/<upstream>.svg` to the registry as `<name>.svg`.
	/// Simple Icons carry no colour, so they get a neutral fill that reads on
	/// light and dark backgrounds.
	fn vendor(&mut self, name: &str, source: &'static str, upstream: &str) -> Result<bool, String> {
		if self.vendored.contains_key(name) {
			return Ok(true);
		}
		let path = self.source_path(source, upstream);
		let Ok(mut svg) = fs::read_to_string(&path) else {
			return Ok(false);
		};
		if source == "si" {
			svg = svg.replacen("<svg ", "<svg fill=\"#737373\" ", 1);
		}
		if svg.len() > super::MAX_LOGO_BYTES || !svg.contains("<svg") {
			return Ok(false);
		}
		io(fs::create_dir_all(&self.opts.statics), "create logos dir")?;
		io(
			fs::write(self.opts.statics.join(format!("{name}.svg")), svg),
			"write logo",
		)?;
		let repo = if source == "di" {
			"homarr-labs/dashboard-icons svg"
		} else {
			"simple-icons/simple-icons icons"
		};
		self.vendored.insert(
			name.to_string(),
			Vendored {
				name: name.to_string(),
				source: if source == "di" {
					"dashboard-icons"
				} else {
					"simple-icons"
				},
				upstream: format!("{repo}/{upstream}.svg"),
			},
		);
		Ok(true)
	}

	/// The curated list.
	fn load_list(&mut self) -> Result<Vec<String>, String> {
		let text = io(fs::read_to_string(&self.opts.logos), "read logos list")?;
		let mut missing = Vec::new();
		for line in text.lines() {
			let line = line.trim();
			if line.is_empty() || line.starts_with('#') {
				continue;
			}
			let parts: Vec<&str> = line.split_whitespace().collect();
			let [name, source, upstream] = parts[..] else {
				return Err(format!("logos list: bad line {line:?}"));
			};
			let source = if source == "di" { "di" } else { "si" };
			if !self.vendor(name, source, upstream)? {
				missing.push(name.to_string());
			}
		}
		Ok(missing)
	}

	/// A logo for a template with none of its own: the same name in either
	/// icon set, else a lettered tile.
	fn fallback(&mut self, id: &str, name: &str) -> Result<String, String> {
		let squashed: String = name
			.to_lowercase()
			.chars()
			.filter(char::is_ascii_alphanumeric)
			.collect();
		let dashed: String = name
			.to_lowercase()
			.split(|c: char| !c.is_ascii_alphanumeric())
			.filter(|s| !s.is_empty())
			.collect::<Vec<_>>()
			.join("-");
		let nodash = id.replace('-', "");
		let candidates = [id.to_string(), dashed, squashed, nodash];
		for c in &candidates {
			if c.is_empty() {
				continue;
			}
			if self.vendored.contains_key(c) {
				return Ok(c.clone());
			}
			for source in ["di", "si"] {
				if self.vendor(c, if source == "di" { "di" } else { "si" }, c)? {
					return Ok(c.clone());
				}
			}
		}
		let mono = format!("mono-{id}");
		io(fs::create_dir_all(&self.opts.statics), "create logos dir")?;
		io(
			fs::write(
				self.opts.statics.join(format!("{mono}.svg")),
				monogram(id, name),
			),
			"write monogram",
		)?;
		Ok(mono)
	}
}

/// A rounded tile with the first letters of the name; the colour comes from
/// the id, so it is the same on every run.
fn monogram(id: &str, name: &str) -> String {
	let hue = id
		.bytes()
		.fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)))
		% 360;
	let letters: String = name
		.split(|c: char| !c.is_alphanumeric())
		.filter(|w| !w.is_empty())
		.filter_map(|w| w.chars().next())
		.take(2)
		.collect::<String>()
		.to_uppercase();
	let letters: String = if letters.is_empty() {
		"?".to_string()
	} else {
		letters
	};
	let escaped = letters.replace('&', "&amp;").replace('<', "&lt;");
	format!(
		"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\" role=\"img\">\
		<rect width=\"64\" height=\"64\" rx=\"14\" fill=\"hsl({hue} 45% 38%)\"/>\
		<text x=\"32\" y=\"33\" text-anchor=\"middle\" dominant-baseline=\"central\" \
		font-family=\"system-ui, sans-serif\" font-size=\"26\" font-weight=\"600\" \
		fill=\"#fff\">{escaped}</text></svg>\n"
	)
}

// ------------------------------------------------------------ conversion

/// Tags that say what a template is, in the order they decide the category.
const CATEGORY_TAGS: &[(&str, &[&str])] = &[
	(
		"ai",
		&[
			"ai",
			"llm",
			"agents",
			"mcp",
			"chatbot",
			"machine-learning",
			"ml",
			"rag",
			"inference",
		],
	),
	("backup", &["backup"]),
	(
		"database",
		&[
			"database",
			"postgres",
			"postgresql",
			"mysql",
			"mariadb",
			"redis",
			"mongodb",
			"sql",
			"nosql",
			"clickhouse",
			"data",
		],
	),
	(
		"monitoring",
		&[
			"monitoring",
			"observability",
			"metrics",
			"logging",
			"uptime",
			"status-page",
			"alerting",
			"tracing",
			"apm",
		],
	),
	(
		"security",
		&[
			"security",
			"authentication",
			"auth",
			"sso",
			"identity",
			"password-manager",
			"privacy",
			"secrets",
		],
	),
	(
		"networking",
		&[
			"networking",
			"proxy",
			"dns",
			"vpn",
			"reverse-proxy",
			"gateway",
			"firewall",
			"network",
		],
	),
	(
		"automation",
		&[
			"automation",
			"workflow",
			"low-code",
			"no-code",
			"integration",
		],
	),
	(
		"analytics",
		&[
			"analytics",
			"business-intelligence",
			"bi",
			"reporting",
			"statistics",
		],
	),
	(
		"cms",
		&[
			"cms",
			"blog",
			"content-management",
			"static-site",
			"website",
		],
	),
	(
		"communication",
		&[
			"chat",
			"messaging",
			"email",
			"communication",
			"whatsapp",
			"voip",
			"forum",
			"community",
			"social",
			"notifications",
		],
	),
	(
		"media",
		&[
			"media",
			"streaming",
			"video",
			"music",
			"photos",
			"podcast",
			"gallery",
			"downloader",
			"torrent",
			"books",
		],
	),
	(
		"storage",
		&[
			"storage",
			"s3",
			"object-storage",
			"file-sharing",
			"file-manager",
			"sync",
			"cloud-storage",
		],
	),
	(
		"development",
		&[
			"development",
			"developer-tools",
			"devops",
			"git",
			"ci-cd",
			"docker",
			"api",
			"code",
			"ide",
			"testing",
			"baas",
			"hosting",
		],
	),
	(
		"finance",
		&[
			"finance",
			"accounting",
			"invoicing",
			"budgeting",
			"payments",
			"banking",
			"crypto",
		],
	),
	("games", &["gaming", "game-server", "minecraft"]),
	(
		"productivity",
		&[
			"productivity",
			"project-management",
			"collaboration",
			"notes",
			"knowledge-base",
			"documentation",
			"bookmarks",
			"scheduling",
			"calendar",
			"erp",
			"crm",
			"business",
			"markdown",
			"rss",
			"dashboard",
		],
	),
	(
		"tools",
		&["utilities", "tools", "converter", "pdf", "search"],
	),
];

fn category_of(tags: &[String]) -> &'static str {
	for tag in tags {
		let tag = tag.to_lowercase();
		for (category, words) in CATEGORY_TAGS {
			if words.contains(&tag.as_str()) {
				return category;
			}
		}
	}
	"other"
}

/// The sentences of Dokploy's marketing description that do not talk about
/// Dokploy.
fn clean_description(text: &str, name: &str) -> String {
	let kept: Vec<&str> = text
		.split_inclusive(". ")
		.map(str::trim)
		.filter(|s| !s.is_empty() && !s.to_lowercase().contains("dokploy"))
		.collect();
	let mut out = kept.join(" ");
	if out.is_empty() {
		out = format!("{name}, ready to deploy.");
	}
	if out.chars().count() > 300 {
		out = out.chars().take(297).collect::<String>() + "...";
	}
	out
}

fn str_of(v: &toml::Value) -> Option<String> {
	match v {
		toml::Value::String(s) => Some(s.clone()),
		toml::Value::Integer(i) => Some(i.to_string()),
		toml::Value::Float(f) => Some(f.to_string()),
		toml::Value::Boolean(b) => Some(b.to_string()),
		_ => None,
	}
}

#[derive(serde::Deserialize, Default)]
struct Meta {
	#[serde(default)]
	name: String,
	#[serde(default)]
	version: String,
	#[serde(default)]
	description: String,
	#[serde(default)]
	logo: String,
	#[serde(default)]
	links: BTreeMap<String, String>,
	#[serde(default)]
	tags: Vec<String>,
}

/// Where a logo comes from.
enum LogoChoice {
	/// A file of the blueprint, copied into the template's folder.
	Own(String, Vec<u8>),
	/// An entry of the registry.
	Registry(String),
}

/// A blueprint turned into podkit files, before the checks.
struct Converted {
	template: Template,
	compose: String,
	logo_file: Option<(String, Vec<u8>)>,
}

#[allow(
	clippy::too_many_lines,
	reason = "reads one blueprint, section by section"
)]
fn convert(
	dir: &Path,
	upstream_id: &str,
	id: &str,
	rev: &str,
	logos: &mut Logos<'_>,
) -> Result<Converted, String> {
	let read =
		|name: &str| fs::read_to_string(dir.join(name)).map_err(|e| format!("no {name}: {e}"));
	let meta: Meta =
		serde_json::from_str(&read("meta.json")?).map_err(|e| format!("meta.json: {e}"))?;
	let compose = read("docker-compose.yml")?;
	let raw: toml::Value = toml::from_str(&read("template.toml")?)
		.map_err(|e| format!("template.toml: {}", e.message()))?;

	let mut variables = BTreeMap::new();
	if let Some(table) = raw.get("variables").and_then(toml::Value::as_table) {
		for (name, value) in table {
			if let Some(text) = str_of(value) {
				variables.insert(name.clone(), Variable::Expr(text));
			}
		}
	}
	let config_raw = raw.get("config");
	let mut config = Config::default();
	if let Some(cfg) = config_raw {
		if let Some(env) = cfg.get("env") {
			config.env = env_of(env)?;
		}
		if let Some(domains) = cfg.get("domains").and_then(toml::Value::as_array) {
			for d in domains {
				let service = d
					.get("serviceName")
					.and_then(toml::Value::as_str)
					.unwrap_or_default();
				if service.is_empty() {
					continue;
				}
				let port = d
					.get("port")
					.and_then(|p| match p {
						toml::Value::Integer(i) => Some(*i),
						toml::Value::String(s) => s.trim().parse().ok(),
						_ => None,
					})
					.and_then(|p| u16::try_from(p).ok())
					.ok_or_else(|| format!("domain of {service}: port is not a number"))?;
				config.domains.push(DomainMap {
					service_name: service.to_string(),
					port,
					host: d
						.get("host")
						.and_then(toml::Value::as_str)
						.map(str::to_string),
					path: d
						.get("path")
						.and_then(toml::Value::as_str)
						.map(str::to_string),
				});
			}
		}
		if let Some(mounts) = cfg.get("mounts").and_then(toml::Value::as_array) {
			for m in mounts {
				let file_path = m
					.get("filePath")
					.and_then(toml::Value::as_str)
					.unwrap_or_default();
				let content = m
					.get("content")
					.and_then(toml::Value::as_str)
					.unwrap_or_default();
				if file_path.is_empty() && content.is_empty() {
					continue;
				}
				// Dokploy lists the directories a compose file binds as mounts with
				// no content. A directory is a named volume here.
				let base = file_path.rsplit('/').next().unwrap_or_default();
				if content.is_empty() && !base.contains('.') {
					continue;
				}
				config.mounts.push(Mount {
					file_path: file_path.to_string(),
					content: content.to_string(),
				});
			}
		}
	}

	let name = if meta.name.is_empty() {
		id.to_string()
	} else {
		meta.name.clone()
	};
	let logo = choose_logo(dir, &meta.logo);
	let (logo_name, logo_file) = match logo {
		LogoChoice::Own(file, bytes) => (file.clone(), Some((file, bytes))),
		LogoChoice::Registry(r) => (r, None),
	};
	let logo_name = if logo_file.is_some() {
		logo_name
	} else {
		let fallback = logos.fallback(id, &name)?;
		format!("@{fallback}")
	};
	let template = Template {
		id: id.to_string(),
		name: name.clone(),
		description: clean_description(&meta.description, &name),
		category: category_of(&meta.tags).to_string(),
		tags: meta.tags.iter().map(|t| t.to_lowercase()).collect(),
		version: meta.version.clone(),
		logo: logo_name,
		website: meta.links.get("website").cloned(),
		docs: meta.links.get("docs").cloned(),
		source: meta.links.get("github").cloned(),
		upstream: Some(format!("Dokploy/templates@{rev} blueprints/{upstream_id}")),
		gateway: false,
		mcp: None,
		requires: Requires::default(),
		reveal: reveal_of(&variables),
		variables,
		config,
	};
	Ok(Converted {
		template,
		compose,
		logo_file,
	})
}

/// Generated passwords for logging in to the app itself (not its database
/// or mail server): the user has no other way to learn them.
fn reveal_of(variables: &BTreeMap<String, Variable>) -> Vec<String> {
	let login = [
		"admin",
		"login",
		"dashboard",
		"superuser",
		"super_user",
		"auth",
		"ui",
		"webui",
		"owner",
		"root_user",
		"initial",
		"default_user",
		"install_admin",
	];
	let not_login = [
		"db",
		"database",
		"redis",
		"postgres",
		"pg",
		"mysql",
		"mongo",
		"smtp",
		"mail",
		"clickhouse",
		"minio",
		"s3",
		"rabbitmq",
		"keydb",
		"valkey",
		"influx",
		"elastic",
		"kibana",
		"api",
		"jwt",
		"secret",
		"encryption",
		"salt",
		"token",
		"mariadb",
		"root_password",
	];
	variables
		.iter()
		.filter_map(|(name, v)| {
			let Variable::Expr(text) = v else {
				return None;
			};
			let n = name.to_lowercase();
			let password = n.ends_with("password") || n.ends_with("_pass") || n.ends_with("_pw");
			let generated =
				text.contains("${password") || text.contains("${base64") || text.contains("${hash");
			let is_login = login
				.iter()
				.any(|w| n.starts_with(w) || n.contains(&format!("_{w}")));
			let other = not_login.iter().any(|w| n.contains(w));
			(password && generated && is_login && !other).then(|| name.clone())
		})
		.collect()
}

fn env_of(v: &toml::Value) -> Result<BTreeMap<String, String>, String> {
	let mut out = BTreeMap::new();
	let put_table = |t: &toml::map::Map<String, toml::Value>,
	                 out: &mut BTreeMap<String, String>| {
		for (k, v) in t {
			if let Some(s) = str_of(v) {
				out.insert(k.clone(), s);
			}
		}
	};
	match v {
		toml::Value::Table(t) => put_table(t, &mut out),
		toml::Value::Array(items) => {
			for item in items {
				match item {
					toml::Value::String(line) => {
						let (k, v) = line.split_once('=').unwrap_or((line.as_str(), ""));
						if !k.trim().is_empty() && !k.trim().starts_with('#') {
							out.insert(k.trim().to_string(), v.to_string());
						}
					}
					toml::Value::Table(t) => put_table(t, &mut out),
					_ => {}
				}
			}
		}
		other => return Err(format!("config.env is a {}", other.type_str())),
	}
	Ok(out)
}

/// The blueprint's own logo if it is an SVG, or a PNG that fits; otherwise
/// the registry stands in.
fn choose_logo(dir: &Path, file: &str) -> LogoChoice {
	if file.is_empty() || file.contains('/') {
		return LogoChoice::Registry(String::new());
	}
	let ext_ok = logo_type(file).is_some();
	let Ok(bytes) = fs::read(dir.join(file)) else {
		return LogoChoice::Registry(String::new());
	};
	if !ext_ok || bytes.is_empty() || bytes.len() > super::MAX_LOGO_BYTES {
		return LogoChoice::Registry(String::new());
	}
	if file.to_ascii_lowercase().ends_with(".svg") {
		let text = String::from_utf8_lossy(&bytes).to_lowercase();
		if !text.contains("<svg") || text.contains("<script") || text.contains("<image") {
			// An SVG that wraps a bitmap is not a vector logo, and a script has no place here.
			return LogoChoice::Registry(String::new());
		}
		return LogoChoice::Own("logo.svg".to_string(), bytes);
	}
	if bytes.starts_with(b"\x89PNG") {
		return LogoChoice::Own("logo.png".to_string(), bytes);
	}
	LogoChoice::Registry(String::new())
}

// ------------------------------------------------------------------ files

fn template_files(c: &Converted) -> Result<Vec<(String, Vec<u8>)>, String> {
	let toml = c.template.to_toml().map_err(|e| e.0)?;
	let mut files = vec![
		("template.toml".to_string(), toml.into_bytes()),
		(
			"docker-compose.yml".to_string(),
			c.compose.clone().into_bytes(),
		),
	];
	if let Some((name, bytes)) = &c.logo_file {
		files.push((name.clone(), bytes.clone()));
	}
	Ok(files)
}

fn check(id: &str, files: &[(String, Vec<u8>)], statics: &Path) -> Result<Loaded, String> {
	let lookup = |name: &str| -> Option<Cow<'static, [u8]>> {
		if let Some(r) = name.strip_prefix('@') {
			return fs::read(statics.join(format!("{r}.svg")))
				.ok()
				.map(Cow::Owned);
		}
		files
			.iter()
			.find(|(n, _)| n == name)
			.map(|(_, b)| Cow::Owned(b.clone()))
	};
	load(id, &lookup).map_err(|e| e.0)
}

/// Removes the folders an earlier import made; hand-written templates have no
/// `upstream` and stay.
fn clear_imported(out: &Path) -> Result<(), String> {
	let Ok(entries) = fs::read_dir(out) else {
		return Ok(());
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let toml = path.join("template.toml");
		if let Ok(text) = fs::read_to_string(&toml)
			&& text.lines().any(|l| l.starts_with("upstream = "))
		{
			io(fs::remove_dir_all(&path), "remove old import")?;
		}
	}
	Ok(())
}

// --------------------------------------------------------- image checks

/// `nginx` -> `docker.io/library/nginx`, `grafana/grafana:11` ->
/// `docker.io/grafana/grafana:11`.
fn qualify(image: &str) -> String {
	let first = image.split('/').next().unwrap_or_default();
	let has_host =
		image.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost");
	if has_host {
		image.to_string()
	} else if image.contains('/') {
		format!("docker.io/{image}")
	} else {
		format!("docker.io/library/{image}")
	}
}

/// `Ok` when the registry has the image, `Err(true)` when it says it does
/// not, `Err(false)` when the check itself failed (network, rate limit).
fn inspect(image: &str) -> Result<(), (bool, String)> {
	let podman = std::env::var("PODMAN").unwrap_or_else(|_| {
		["/run/current-system/sw/bin/podman"]
			.into_iter()
			.find(|p| {
				Path::new(p).exists() && Command::new("podman").arg("--version").output().is_err()
			})
			.unwrap_or("podman")
			.to_string()
	});
	let out = Command::new(podman)
		.args(["manifest", "inspect", &qualify(image)])
		.output()
		.map_err(|e| (false, format!("podman: {e}")))?;
	if out.status.success() {
		return Ok(());
	}
	let text = String::from_utf8_lossy(&out.stderr).to_string();
	let lower = text.to_lowercase();
	let definite = lower.contains("manifest unknown")
		|| lower.contains("name unknown")
		|| lower.contains("not found")
		|| lower.contains("requested access to the resource is denied");
	Err((
		definite,
		text.trim().lines().last().unwrap_or_default().to_string(),
	))
}

/// Checks every image with `podman manifest inspect`, eight at a time.
fn verify(images: &BTreeSet<String>) -> BTreeMap<String, Result<(), (bool, String)>> {
	let queue: Mutex<Vec<String>> = Mutex::new(images.iter().cloned().collect());
	let results = Mutex::new(BTreeMap::new());
	std::thread::scope(|scope| {
		for _ in 0..8 {
			scope.spawn(|| {
				loop {
					let next = queue.lock().expect("queue").pop();
					let Some(image) = next else { break };
					let r = inspect(&image);
					results.lock().expect("results").insert(image, r);
				}
			});
		}
	});
	results.into_inner().expect("results")
}

// ---------------------------------------------------------------- the run

struct Skipped {
	id: String,
	reason: String,
}

/// A short, countable reason from a long message.
fn reason_class(reason: &str) -> String {
	let r = reason.to_lowercase();
	let table = [
		(
			"completed_successfully",
			"init container (depends_on service_completed_successfully)",
		),
		(
			"service_completed",
			"init container (depends_on service_completed_successfully)",
		),
		("docker.sock", "mounts the container socket"),
		("podman.sock", "mounts the container socket"),
		("build", "build: instead of image:"),
		("absolute host", "host bind mount"),
		("host path", "host bind mount"),
		("routing by path", "routing by path"),
		("privileged", "privileged"),
		("devices", "devices"),
		("port range", "port range"),
		("network_mode", "network_mode"),
		("is not supported", "unsupported helper"),
		("env_file", "env_file"),
		("driver", "volume driver"),
		("capabilit", "dangerous capability"),
		("cap_add", "dangerous capability"),
	];
	for (needle, label) in table {
		if r.contains(needle) {
			return label.to_string();
		}
	}
	"other".to_string()
}

#[allow(
	clippy::too_many_lines,
	reason = "the import is one pass: convert, check, verify, write"
)]
fn run(o: &Opts) -> Result<(), String> {
	let mut logos = Logos::new(o);
	let missing = logos.load_list()?;
	if !missing.is_empty() {
		eprintln!("logos not found in the icon sets: {missing:?}");
	}

	io(fs::create_dir_all(&o.out), "create output dir")?;
	clear_imported(&o.out)?;

	let mut ids: Vec<String> = io(fs::read_dir(&o.dokploy), "read blueprints")?
		.flatten()
		.filter(|e| e.path().is_dir())
		.filter_map(|e| e.file_name().into_string().ok())
		.collect();
	ids.sort();

	let own_ids = own_templates(&o.out);
	let mut kept: Vec<(Converted, Loaded)> = Vec::new();
	let mut skipped = Vec::new();
	for id in &ids {
		let dir = o.dokploy.join(id);
		// `odoo_17` and `poste.io` become `odoo-17` and `poste-io`; the
		// upstream path keeps the original name.
		let tid: String = id
			.to_lowercase()
			.chars()
			.map(|c| if c == '_' || c == '.' { '-' } else { c })
			.collect();
		if own_ids.contains(&tid) {
			skipped.push(Skipped {
				id: id.clone(),
				reason: format!("replaced by the hand-written template {tid}"),
			});
			continue;
		}
		if !valid_id(&tid) {
			skipped.push(Skipped {
				id: id.clone(),
				reason: "id is not a valid template id".to_string(),
			});
			continue;
		}
		let result = convert(
			&dir,
			id,
			&tid,
			&o.dokploy_rev[..o.dokploy_rev.len().min(7)],
			&mut logos,
		)
		.and_then(|c| {
			let files = template_files(&c)?;
			// The serialized text is what gets loaded, so the check sees what ships.
			let loaded = check(&tid, &files, &o.statics)?;
			Ok((c, loaded))
		});
		match result {
			Ok(pair) => kept.push(pair),
			Err(reason) => skipped.push(Skipped {
				id: id.clone(),
				reason,
			}),
		}
	}

	// Images.
	let mut image_notes: BTreeMap<String, String> = BTreeMap::new();
	if o.verify != Verify::No {
		let mut images = BTreeSet::new();
		for (_, l) in &kept {
			images.extend(l.services.iter().map(|s| s.image.clone()));
		}
		let own = own_templates(&o.out);
		for id in &own {
			if let Some(l) = load_dir(&o.out.join(id), &o.statics) {
				images.extend(l.services.iter().map(|s| s.image.clone()));
			}
		}
		if o.verify == Verify::Own {
			images.retain(|i| own_images(&o.out, &o.statics, &own).contains(i));
		}
		eprintln!(
			"verifying {} images with podman manifest inspect",
			images.len()
		);
		let results = verify(&images);
		let mut dropped = BTreeSet::new();
		for (image, r) in &results {
			if let Err((definite, why)) = r {
				if *definite {
					image_notes.insert(image.clone(), why.clone());
					dropped.insert(image.clone());
				} else {
					eprintln!("could not check {image}: {why}");
				}
			}
		}
		// A hand-written template's images must be confirmed, not merely not
		// refuted: a check that could not run is an error too.
		let own_set = own_images(&o.out, &o.statics, &own);
		let own_bad: Vec<(&String, &String)> = results
			.iter()
			.filter(|(i, _)| own_set.contains(*i))
			.filter_map(|(i, r)| r.as_ref().err().map(|(_, why)| (i, why)))
			.collect();
		if !own_bad.is_empty() {
			return Err(format!(
				"hand-written templates use images that do not exist: {own_bad:?}"
			));
		}
		let mut still = Vec::new();
		for (c, l) in kept {
			if let Some(bad) = l.services.iter().find(|s| dropped.contains(&s.image)) {
				skipped.push(Skipped {
					id: c.template.id.clone(),
					reason: format!(
						"image {} does not exist: {}",
						bad.image, image_notes[&bad.image]
					),
				});
			} else {
				still.push((c, l));
			}
		}
		kept = still;
	}

	for (c, _) in &kept {
		let dir = o.out.join(&c.template.id);
		io(fs::create_dir_all(&dir), "create template dir")?;
		for (name, bytes) in template_files(c)? {
			io(fs::write(dir.join(&name), bytes), "write template file")?;
		}
	}
	skipped.sort_by(|a, b| a.id.cmp(&b.id));
	write_report(o, ids.len(), &kept, &skipped, &logos)?;
	write_notice(o, &logos)?;
	write_sources(o, &logos)?;
	eprintln!("imported {} of {} blueprints", kept.len(), ids.len());
	Ok(())
}

fn own_templates(out: &Path) -> Vec<String> {
	let mut ids = Vec::new();
	if let Ok(entries) = fs::read_dir(out) {
		for entry in entries.flatten() {
			let text = fs::read_to_string(entry.path().join("template.toml")).unwrap_or_default();
			if !text.is_empty()
				&& !text.lines().any(|l| l.starts_with("upstream = "))
				&& let Ok(name) = entry.file_name().into_string()
			{
				ids.push(name);
			}
		}
	}
	ids.sort();
	ids
}

fn own_images(out: &Path, statics: &Path, own: &[String]) -> BTreeSet<String> {
	own.iter()
		.filter_map(|id| load_dir(&out.join(id), statics))
		.flat_map(|l| l.services.into_iter().map(|s| s.image))
		.collect()
}

fn load_dir(dir: &Path, statics: &Path) -> Option<Loaded> {
	let id = dir.file_name()?.to_str()?.to_string();
	let statics = statics.to_path_buf();
	let dir = dir.to_path_buf();
	let lookup = move |name: &str| -> Option<Cow<'static, [u8]>> {
		if let Some(r) = name.strip_prefix('@') {
			return fs::read(statics.join(format!("{r}.svg")))
				.ok()
				.map(Cow::Owned);
		}
		fs::read(dir.join(name)).ok().map(Cow::Owned)
	};
	load(&id, &lookup).ok()
}

fn write_report(
	o: &Opts,
	total: usize,
	kept: &[(Converted, Loaded)],
	skipped: &[Skipped],
	logos: &Logos<'_>,
) -> Result<(), String> {
	let own = own_templates(&o.out);
	let mut classes: BTreeMap<String, usize> = BTreeMap::new();
	for s in skipped {
		*classes.entry(reason_class(&s.reason)).or_default() += 1;
	}
	let single = kept.iter().filter(|(_, l)| l.plain_app).count();
	let own_logo = kept.iter().filter(|(c, _)| c.logo_file.is_some()).count();
	let mut r = String::new();
	let _ = writeln!(r, "# Template import report\n");
	let _ = writeln!(
		r,
		"Generated by `scripts/templates-import`. Do not edit by hand.\n\n\
		Source: [Dokploy/templates](https://github.com/Dokploy/templates) at `{}` (MIT, see `NOTICE`).\n\
		Kept: the blueprints that parse, whose logo is usable, and whose compose file passes the \
		compose parser and the stack translation of `crates/core/src/stacks` with placeholder values.\n",
		o.dokploy_rev
	);
	let _ = writeln!(r, "## Counts\n\n| | |\n|---|---:|");
	let _ = writeln!(r, "| blueprints in the source | {total} |");
	let _ = writeln!(r, "| imported | {} |", kept.len());
	let _ = writeln!(r, "| skipped | {} |", skipped.len());
	let _ = writeln!(r, "| imported as an image application | {single} |");
	let _ = writeln!(r, "| imported as a stack | {} |", kept.len() - single);
	let _ = writeln!(r, "| with their own logo | {own_logo} |");
	let _ = writeln!(r, "| with a registry logo | {} |", kept.len() - own_logo);
	let _ = writeln!(r, "| hand-written (not from Dokploy) | {} |", own.len());
	let _ = writeln!(
		r,
		"| B3 spike: fully supported (docs/spikes/compose-coverage.json) | 433 |"
	);
	let _ = writeln!(
		r,
		"| imported / 433 | {:.1}% |",
		kept.len() as f64 * 100.0 / 433.0
	);
	let _ = writeln!(
		r,
		"\n## Why the rest were skipped\n\n| Reason | Templates |\n|---|---:|"
	);
	let mut by_count: Vec<_> = classes.iter().collect();
	by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
	for (class, n) in by_count {
		let _ = writeln!(r, "| {class} | {n} |");
	}
	let _ = writeln!(
		r,
		"\n### Skipped, one by one\n\n| Template | Reason |\n|---|---|"
	);
	for s in skipped {
		let reason: String = s.reason.replace('|', "\\|").replace('\n', " ");
		let reason: String = reason.chars().take(300).collect();
		let _ = writeln!(r, "| `{}` | {reason} |", s.id);
	}
	let _ = writeln!(
		r,
		"\n## Imported, with their upstream path\n\n| Template | Upstream | Kind |\n|---|---|---|"
	);
	for (c, l) in kept {
		let _ = writeln!(
			r,
			"| `{}` | `{}` | {} |",
			c.template.id,
			c.template.upstream.as_deref().unwrap_or_default(),
			if l.plain_app { "application" } else { "stack" }
		);
	}
	let _ = writeln!(r, "\n## Registry logos used by templates\n");
	let _ = writeln!(
		r,
		"{} files in `crates/core/static/logos`, see `SOURCES.tsv` there.",
		logos.vendored.len()
	);
	io(fs::write(o.out.join("IMPORT_REPORT.md"), r), "write report")
}

fn write_sources(o: &Opts, logos: &Logos<'_>) -> Result<(), String> {
	let mut s = String::from("name\tsource\tupstream path\n");
	for v in logos.vendored.values() {
		let _ = writeln!(s, "{}\t{}\t{}", v.name, v.source, v.upstream);
	}
	let _ = writeln!(
		s,
		"# mono-<id>.svg are generated tiles, not from any source."
	);
	io(
		fs::write(o.statics.join("SOURCES.tsv"), s),
		"write SOURCES.tsv",
	)
}

fn write_notice(o: &Opts, logos: &Logos<'_>) -> Result<(), String> {
	let di = logos
		.vendored
		.values()
		.filter(|v| v.source == "dashboard-icons")
		.count();
	let si = logos
		.vendored
		.values()
		.filter(|v| v.source == "simple-icons")
		.count();
	let mut n = String::new();
	let _ = writeln!(
		n,
		"podkit includes third-party material under the following terms.\n"
	);
	let _ = writeln!(
		n,
		"== Dokploy templates (crates/core/templates/*, `upstream` in each template.toml) ==\n"
	);
	let _ = writeln!(
		n,
		"Templates marked `upstream = \"Dokploy/templates@{} ...\"` are converted from the blueprints of\n\
		https://github.com/Dokploy/templates. Each one's path in that repository is in its template.toml\n\
		and in crates/core/templates/IMPORT_REPORT.md.\n",
		&o.dokploy_rev[..o.dokploy_rev.len().min(7)]
	);
	let _ = writeln!(n, "MIT License\n\n{DOKPLOY_COPYRIGHT}\n\n{MIT}\n");
	let _ = writeln!(
		n,
		"== Logos (crates/core/static/logos, templates' own logo files) ==\n"
	);
	let _ = writeln!(
		n,
		"{di} SVG files come from https://github.com/homarr-labs/dashboard-icons at {} (Apache License 2.0,\n\
		https://www.apache.org/licenses/LICENSE-2.0). {si} come from https://github.com/simple-icons/simple-icons\n\
		at {} (CC0 1.0 Universal, https://creativecommons.org/publicdomain/zero/1.0/). Each file's origin is\n\
		in crates/core/static/logos/SOURCES.tsv; Simple Icons files had a neutral `fill` added.\n\n\
		The logo files that ship inside imported templates were published by the Dokploy/templates\n\
		repository (MIT, above). The logos remain trademarks of their owners and appear only to\n\
		identify the software a template installs. Files named mono-<id>.svg were generated for podkit.",
		o.di_rev, o.si_rev
	);
	io(fs::write(&o.notice, n), "write NOTICE")
}
