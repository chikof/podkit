//! One-click templates: a service or a small stack with its variables, ready
//! to deploy from a gallery.
//!
//! Templates are files under `server/templates/<id>/` that `build.rs` embeds
//! in the binary, so nothing is fetched at run time:
//!
//! ```text
//! template.toml       who it is, its variables, domains and file mounts
//! docker-compose.yml  what runs (the subset of `docs/stacks.md`)
//! <logo>              an SVG, or a PNG of at most 64 KiB
//! ```
//!
//! `template.toml` keeps the shape of Dokploy's blueprints (MIT), so those
//! import without rewriting; see `docs/templates.md`.

pub(crate) mod embed;
pub mod expr;
pub mod import;
mod registry;
pub mod render;
mod routes;
pub mod service;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

pub use registry::{Loaded, find, short_hash};
#[cfg(test)]
pub use registry::{load_all, search};
pub use routes::{deploy_router, router};

/// Categories the gallery groups templates by.
pub const CATEGORIES: &[&str] = &[
	"ai",
	"analytics",
	"automation",
	"backup",
	"cms",
	"communication",
	"database",
	"development",
	"finance",
	"games",
	"media",
	"monitoring",
	"networking",
	"productivity",
	"security",
	"storage",
	"tools",
	"other",
];

/// Largest logo, in bytes.
pub const MAX_LOGO_BYTES: usize = 64 * 1024;

/// A template as written in `template.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Template {
	/// Folder name: letters, digits and `-`.
	pub id: String,
	/// Name in the gallery.
	pub name: String,
	/// One or two sentences.
	pub description: String,
	/// One of [`CATEGORIES`].
	pub category: String,
	/// Free-form search words.
	#[serde(default)]
	pub tags: Vec<String>,
	/// Version of the software the images carry.
	#[serde(default)]
	pub version: String,
	/// File name of the logo inside the template's folder.
	pub logo: String,
	/// Project home page.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub website: Option<String>,
	/// Documentation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub docs: Option<String>,
	/// Source repository.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
	/// Where an imported template came from, e.g.
	/// `Dokploy/templates@6313a2e blueprints/umami`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub upstream: Option<String>,
	/// The software talks to an OpenAI-compatible endpoint: link a podkit
	/// model to the application and it gets the gateway's variables.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub gateway: bool,
	/// What the host needs to run it.
	#[serde(default, skip_serializing_if = "Requires::is_empty")]
	pub requires: Requires,
	/// Generated variables the user needs to log in (an admin password), by
	/// name. The deploy response shows their values once; nothing else
	/// generated is ever shown.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub reveal: Vec<String>,
	/// Values the template computes or asks for, by name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub variables: BTreeMap<String, Variable>,
	/// Environment, domains and files.
	#[serde(default)]
	pub config: Config,
}

/// Minimum host resources.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Requires {
	/// Free memory the software needs, in MiB.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub ram_mb: Option<u32>,
}

impl Requires {
	fn is_empty(&self) -> bool {
		self.ram_mb.is_none()
	}
}

/// A variable: text with `${...}` expressions, or a question for the user.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Variable {
	/// `"${password:32}"`, `"${main_domain}/path"`, `"fixed"`.
	Expr(String),
	/// A value the user may set when deploying.
	Input(Input),
}

/// A variable the user answers.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Input {
	/// Only `string` exists.
	#[serde(rename = "type")]
	pub kind: String,
	/// Used when the user leaves it empty; without one the user must answer.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub default: Option<String>,
	/// Short name for a form.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub label: Option<String>,
	/// Help text for a form.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
	/// Credential: stored encrypted, never shown again.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub secret: bool,
}

/// The `[config]` table.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
	/// Variables the compose file interpolates (`${NAME}`), after expansion.
	#[serde(
		default,
		deserialize_with = "env_map",
		skip_serializing_if = "BTreeMap::is_empty"
	)]
	pub env: BTreeMap<String, String>,
	/// Which services are reachable from outside.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub domains: Vec<DomainMap>,
	/// Files the compose file mounts, with their content.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub mounts: Vec<Mount>,
}

/// Routes a hostname to a service port.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomainMap {
	/// Compose service.
	#[serde(rename = "serviceName")]
	pub service_name: String,
	/// Container port.
	pub port: u16,
	/// Hostname, usually `${some_domain_variable}`; the service's default
	/// hostname when missing.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub host: Option<String>,
	/// Only `/` is supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub path: Option<String>,
}

/// A file with content.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Mount {
	/// Path relative to the compose file, `./config/app.conf`.
	#[serde(rename = "filePath")]
	pub file_path: String,
	/// Content; may use `${...}`.
	pub content: String,
}

/// `env` as a table, or as Dokploy writes it: a list of `KEY=value` strings
/// and one-entry tables.
fn env_map<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Scalar {
		Str(String),
		Int(i64),
		Float(f64),
		Bool(bool),
	}
	impl Scalar {
		fn text(self) -> String {
			match self {
				Self::Str(s) => s,
				Self::Int(i) => i.to_string(),
				Self::Float(f) => f.to_string(),
				Self::Bool(b) => b.to_string(),
			}
		}
	}
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Item {
		Line(String),
		Table(BTreeMap<String, Scalar>),
	}
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Env {
		Table(BTreeMap<String, Scalar>),
		List(Vec<Item>),
	}
	let mut out = BTreeMap::new();
	match Env::deserialize(d)? {
		Env::Table(t) => out.extend(t.into_iter().map(|(k, v)| (k, v.text()))),
		Env::List(items) => {
			for item in items {
				match item {
					Item::Line(line) => {
						let (k, v) = line.split_once('=').unwrap_or((line.as_str(), ""));
						out.insert(k.trim().to_string(), v.to_string());
					}
					Item::Table(t) => out.extend(t.into_iter().map(|(k, v)| (k, v.text()))),
				}
			}
		}
	}
	Ok(out)
}

/// Why a template is not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

impl Invalid {
	fn new(message: impl Into<String>) -> Self {
		Self(message.into())
	}
}

/// A valid template id: `[a-z0-9]` and inner `-`, at most 50 characters.
#[must_use]
pub fn valid_id(id: &str) -> bool {
	let b = id.as_bytes();
	!b.is_empty()
		&& b.len() <= 50
		&& b[0] != b'-'
		&& b[b.len() - 1] != b'-'
		&& b.iter()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl Template {
	/// Parses `template.toml`.
	///
	/// # Errors
	/// Not TOML, an unknown key, or an unknown variable type.
	pub fn parse(text: &str) -> Result<Self, Invalid> {
		let template: Self = toml::from_str(text).map_err(|e| Invalid::new(e.message()))?;
		template.check_fields()?;
		Ok(template)
	}

	/// The text of `template.toml` for this template.
	///
	/// # Errors
	/// Never, for a template that parsed.
	pub fn to_toml(&self) -> Result<String, Invalid> {
		toml::to_string_pretty(self).map_err(|e| Invalid::new(e.to_string()))
	}

	/// Checks what needs no compose file: names, categories, variables,
	/// expressions, domain paths and mount paths.
	#[allow(
		clippy::too_many_lines,
		reason = "one check after another, each its own message"
	)]
	fn check_fields(&self) -> Result<(), Invalid> {
		if !valid_id(&self.id) {
			return Err(Invalid::new(format!(
				"id {:?} must be lowercase letters, digits and - (at most 50)",
				self.id
			)));
		}
		if self.name.trim().is_empty() || self.name.len() > 80 {
			return Err(Invalid::new("name must be 1 to 80 characters"));
		}
		if self.description.trim().is_empty() {
			return Err(Invalid::new("description is empty"));
		}
		if !CATEGORIES.contains(&self.category.as_str()) {
			return Err(Invalid::new(format!(
				"category {:?} is not one of {CATEGORIES:?}",
				self.category
			)));
		}
		if self.logo.is_empty() || self.logo.contains('/') || self.logo.contains("..") {
			return Err(Invalid::new("logo must be a file name"));
		}
		if self.logo.starts_with('@')
			&& !self.logo[1..]
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
		{
			return Err(Invalid::new("logo must name a registry logo"));
		}
		for (name, var) in &self.variables {
			let ok = !name.is_empty()
				&& name.len() <= 64
				&& name
					.chars()
					.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
			if !ok {
				return Err(Invalid::new(format!("variable name {name:?} is not valid")));
			}
			match var {
				Variable::Expr(text) => {
					expr::scan(text).map_err(|e| Invalid::new(format!("variable {name}: {e}")))?;
				}
				Variable::Input(input) => {
					if input.kind != "string" {
						return Err(Invalid::new(format!(
							"variable {name}: unknown type {:?}; only \"string\" can be asked for",
							input.kind
						)));
					}
				}
			}
		}
		for name in &self.reveal {
			if !matches!(self.variables.get(name), Some(Variable::Expr(_))) {
				return Err(Invalid::new(format!(
					"reveal: {name:?} is not a generated variable"
				)));
			}
		}
		let mut routed = std::collections::BTreeSet::new();
		for domain in &self.config.domains {
			if !routed.insert(domain.service_name.as_str()) {
				return Err(Invalid::new(format!(
					"service {} has more than one domain; a service has one",
					domain.service_name
				)));
			}
		}
		for (key, value) in &self.config.env {
			let ok = key
				.chars()
				.next()
				.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
				&& key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
			if !ok {
				return Err(Invalid::new(format!("env name {key:?} is not valid")));
			}
			expr::scan(value).map_err(|e| Invalid::new(format!("env {key}: {e}")))?;
		}
		for domain in &self.config.domains {
			if domain.port == 0 {
				return Err(Invalid::new(format!(
					"domain of {}: port 0",
					domain.service_name
				)));
			}
			if domain
				.path
				.as_deref()
				.is_some_and(|p| !matches!(p, "" | "/"))
			{
				return Err(Invalid::new(format!(
					"domain of {}: routing by path is not supported",
					domain.service_name
				)));
			}
			if let Some(host) = &domain.host {
				expr::scan(host).map_err(|e| Invalid::new(format!("domain host: {e}")))?;
			}
		}
		for mount in &self.config.mounts {
			if mount_key(&mount.file_path).is_none() {
				return Err(Invalid::new(format!(
					"mount path {:?} must be relative and stay inside the template",
					mount.file_path
				)));
			}
			expr::scan(&mount.content)
				.map_err(|e| Invalid::new(format!("mount {}: {e}", mount.file_path)))?;
		}
		Ok(())
	}
}

/// The key under which the compose parser looks a mounted file up, or `None`
/// for a path that is absolute or climbs out.
#[must_use]
pub fn mount_key(file_path: &str) -> Option<String> {
	let key = crate::stacks::compose::normalize_relative(file_path.trim_start_matches('/'));
	let bad = key.is_empty()
		|| key.starts_with('/')
		|| key.split('/').any(|part| part == ".." || part.is_empty());
	(!bad).then_some(key)
}

/// A logo's media type, from its file name.
#[must_use]
pub fn logo_type(name: &str) -> Option<&'static str> {
	match name.rsplit('.').next()?.to_ascii_lowercase().as_str() {
		"svg" => Some("image/svg+xml"),
		"png" => Some("image/png"),
		_ => None,
	}
}
