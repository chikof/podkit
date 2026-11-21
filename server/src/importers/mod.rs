//! Importers: Coolify, Dokploy and CapRover to a `podkit.toml`.
//!
//! An importer reads a running instance of another platform through its own API (read-only:
//! it only ever sends `GET`, apart from CapRover's login, which is a `POST` that changes
//! nothing), and maps what it finds to the infrastructure-as-code document of [`crate::iac`].
//! What comes out is a [`Imported`]: the [`Doc`], the secrets that go with it, and a
//! [`Report`] of everything that did not map. From there it is the same road as any file:
//! plan against an empty team, then apply.
//!
//! - [`http`] fetches: one [`http::Client`] per source, with a size and time limit, no
//!   redirects, and the credential only ever in a header.
//! - [`coolify`], [`dokploy`] and [`caprover`] each turn the recorded responses of their
//!   source (a [`Snapshot`]) into an [`Imported`]. The mapping is plain data in, plain data
//!   out, so the tests feed it fixtures.
//! - [`service`] authorizes, plans and applies, for the JSON API and the web UI alike.
//!
//! Secrets are never in the document: every environment variable and every registry or git
//! credential is a `${secret:NAME}` placeholder there, and its value is in [`Imported::secrets`],
//! which an apply hands to podkit's encrypted store.

pub mod caprover;
pub mod coolify;
pub mod dokploy;
pub mod http;
mod routes;
pub mod service;

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::iac::doc::{AppDoc, DatabaseDoc, Doc, EnvironmentDoc, ProjectDoc, StackDoc};
use crate::iac::objects::{Secrets, names};

pub use routes::router;

#[cfg(test)]
mod tests;

/// A platform podkit can import from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
	Coolify,
	Dokploy,
	Caprover,
}

impl Source {
	pub const ALL: [Self; 3] = [Self::Coolify, Self::Dokploy, Self::Caprover];

	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Coolify => "coolify",
			Self::Dokploy => "dokploy",
			Self::Caprover => "caprover",
		}
	}

	/// How the platform writes its own name.
	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Coolify => "Coolify",
			Self::Dokploy => "Dokploy",
			Self::Caprover => "CapRover",
		}
	}

	/// What the credential is called on that platform.
	#[must_use]
	pub fn credential(self) -> &'static str {
		match self {
			Self::Coolify => "API token",
			Self::Dokploy => "API key",
			Self::Caprover => "Password",
		}
	}

	#[must_use]
	pub fn parse(name: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|s| s.as_str() == name)
	}
}

/// What the person told podkit about the source.
#[derive(Clone)]
pub struct Credentials {
	/// The address of the instance, `https://coolify.example.com`.
	pub url: String,
	/// A Coolify API token (with `read:sensitive` to read variable values), a Dokploy API key,
	/// or the CapRover password.
	pub token: String,
}

impl std::fmt::Debug for Credentials {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Credentials")
			.field("url", &self.url)
			.field("token", &"(secret)")
			.finish()
	}
}

/// What happened to a setting or an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
	/// A setting podkit has no field for: it was left out.
	Unmapped,
	/// A whole object was not imported.
	Skipped,
	/// Imported, but worth a look before it runs.
	Check,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
	pub level: Level,
	/// Where: `project/environment/name`, or empty for the import as a whole.
	pub at: String,
	pub message: String,
}

/// Everything that did not map, and the counts of what did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
	pub notes: Vec<Note>,
	pub counts: Counts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
	pub projects: usize,
	pub environments: usize,
	pub apps: usize,
	pub databases: usize,
	pub stacks: usize,
	pub variables: usize,
	pub domains: usize,
	pub volumes: usize,
}

impl Report {
	/// Notes of one level.
	pub fn of(&self, level: Level) -> impl Iterator<Item = &Note> {
		self.notes.iter().filter(move |n| n.level == level)
	}

	/// The report as lines of text, for the command line: `Coolify: custom labels are not
	/// supported (shop/production/web)`.
	#[must_use]
	pub fn lines(&self, source: Source) -> Vec<String> {
		self.notes
			.iter()
			.map(|n| {
				let label = match n.level {
					Level::Unmapped => "not supported",
					Level::Skipped => "skipped",
					Level::Check => "check",
				};
				if n.at.is_empty() {
					format!("{}: {} [{label}]", source.label(), n.message)
				} else {
					format!("{}: {} [{label}] ({})", source.label(), n.message, n.at)
				}
			})
			.collect()
	}
}

/// What an importer produces.
#[derive(Debug, Clone)]
pub struct Imported {
	pub source: Source,
	pub doc: Doc,
	/// The values behind the document's `${secret:NAME}` placeholders.
	pub secrets: Secrets,
	pub report: Report,
}

/// The responses of a source, by the request that fetched them (its path and query).
///
/// This is what the importers read, and what the tests record: a fixture is a JSON object of
/// `"path": response`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot(pub BTreeMap<String, Value>);

impl Snapshot {
	#[must_use]
	pub fn get(&self, request: &str) -> Option<&Value> {
		self.0.get(request)
	}

	/// An array response, or nothing.
	#[must_use]
	pub fn list(&self, request: &str) -> &[Value] {
		self.0
			.get(request)
			.and_then(Value::as_array)
			.map_or(&[], Vec::as_slice)
	}
}

/// What the mapping needs to know that the source cannot say.
#[derive(Debug, Clone)]
pub struct Options {
	/// The name of the podkit server everything lands on.
	pub server: String,
}

// ----- the builder every importer writes into

/// A secret's name in the secrets file: unique across the whole import.
fn secret_name(project: &str, env: &str, resource: &str, key: &str) -> String {
	names::env_var(&format!("{project}/{env}"), resource, key)
}

/// What an application is, before its slug and secrets are decided.
pub struct AppSpec {
	/// What the application was called on the source.
	pub name: String,
	pub app: AppDoc,
	/// Variables, in order: every value becomes a secret.
	pub vars: Vec<(String, String)>,
	/// Secrets that are not variables: `git_token`, `registry_username`, `registry_password`.
	pub credentials: Vec<(&'static str, String)>,
}

/// What a compose stack is, before its secrets are decided.
pub struct StackSpec {
	pub name: String,
	pub stack: StackDoc,
	pub vars: Vec<(String, String)>,
}

/// Where an object goes.
#[derive(Debug, Clone)]
pub struct Place {
	pub project: String,
	pub env: String,
}

impl Place {
	#[must_use]
	pub fn path(&self, name: &str) -> String {
		format!("{}/{}/{name}", self.project, self.env)
	}
}

pub struct Builder {
	source: Source,
	pub server: String,
	doc: Doc,
	secrets: BTreeMap<String, String>,
	report: Report,
	/// Public subdomains in use, per server (there is one server).
	slugs: BTreeSet<String>,
	/// Hostnames in use: a host belongs to one application.
	hosts: BTreeSet<String>,
}

/// Lowercase letters, digits and single hyphens, at most 60.
#[must_use]
pub fn slugify(name: &str) -> String {
	crate::networks::slugify(name).unwrap_or_else(|| "imported".to_owned())
}

/// `base`, or `base-2`, `base-3` … until `taken` does not have it.
fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
	if !taken(base) {
		return base.to_owned();
	}
	(2..)
		.map(|n| {
			let suffix = format!("-{n}");
			let room = 60usize.saturating_sub(suffix.len());
			let stem: String = base.chars().take(room).collect();
			format!("{}{suffix}", stem.trim_end_matches('-'))
		})
		.find(|c| !taken(c))
		.unwrap_or_else(|| base.to_owned())
}

impl Builder {
	#[must_use]
	pub fn new(source: Source, options: &Options) -> Self {
		Self {
			source,
			server: options.server.clone(),
			doc: Doc::new(),
			secrets: BTreeMap::new(),
			report: Report::default(),
			slugs: BTreeSet::new(),
			hosts: BTreeSet::new(),
		}
	}

	pub fn note(&mut self, level: Level, at: &str, message: impl Into<String>) {
		let note = Note {
			level,
			at: at.to_owned(),
			message: message.into(),
		};
		if !self.report.notes.contains(&note) {
			self.report.notes.push(note);
		}
	}

	/// A setting the source has and podkit has not.
	pub fn unmapped(&mut self, at: &str, message: impl Into<String>) {
		self.note(Level::Unmapped, at, message);
	}

	/// An object that was left out.
	pub fn skipped(&mut self, at: &str, message: impl Into<String>) {
		self.note(Level::Skipped, at, message);
	}

	/// Imported, but see to it.
	pub fn check(&mut self, at: &str, message: impl Into<String>) {
		self.note(Level::Check, at, message);
	}

	/// A project, by the name it had. Returns its slug in the document.
	pub fn project(&mut self, name: &str) -> String {
		let slug = unique(&slugify(name), |s| self.doc.projects.contains_key(s));
		self.doc.projects.insert(
			slug.clone(),
			ProjectDoc {
				name: name.trim().to_owned(),
				environments: BTreeMap::new(),
				networks: Vec::new(),
			},
		);
		self.report.counts.projects += 1;
		slug
	}

	/// An environment of a project.
	pub fn environment(&mut self, project: &str, name: &str) -> Place {
		let Some(p) = self.doc.projects.get_mut(project) else {
			return Place {
				project: project.to_owned(),
				env: slugify(name),
			};
		};
		let slug = unique(&slugify(name), |s| p.environments.contains_key(s));
		p.environments.insert(
			slug.clone(),
			EnvironmentDoc {
				name: name.trim().to_owned(),
				apps: BTreeMap::new(),
				databases: BTreeMap::new(),
				stacks: BTreeMap::new(),
				models: BTreeMap::new(),
				cron: BTreeMap::new(),
			},
		);
		self.report.counts.environments += 1;
		Place {
			project: project.to_owned(),
			env: slug,
		}
	}

	fn env_mut(&mut self, at: &Place) -> Option<&mut EnvironmentDoc> {
		self.doc
			.projects
			.get_mut(&at.project)?
			.environments
			.get_mut(&at.env)
	}

	/// Every value becomes a secret; the variables of the document are placeholders. A name
	/// that is not a valid variable name is left out and said so.
	fn placeholders(
		&mut self,
		at: &Place,
		resource: &str,
		shown_at: &str,
		vars: Vec<(String, String)>,
	) -> BTreeMap<String, String> {
		let mut out = BTreeMap::new();
		for (key, value) in vars {
			if !crate::validate::env_key(&key) {
				self.skipped(
					shown_at,
					format!("the variable \"{key}\" is not a valid name and was left out"),
				);
				continue;
			}
			let name = secret_name(&at.project, &at.env, resource, &key);
			out.insert(key, names::placeholder(&name));
			self.secrets.insert(name, value);
			self.report.counts.variables += 1;
		}
		out
	}

	/// The hostnames of `wanted` nobody else has, lowercased and sorted.
	fn claim_hosts(&mut self, shown_at: &str, wanted: Vec<String>) -> Vec<String> {
		let mut out = BTreeSet::new();
		for host in wanted {
			let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
			if !crate::validate::hostname(&host) {
				self.skipped(
					shown_at,
					format!("\"{host}\" is not a hostname podkit can route"),
				);
				continue;
			}
			if !self.hosts.insert(host.clone()) {
				self.skipped(
					shown_at,
					format!("\"{host}\" is already used by another imported application"),
				);
				continue;
			}
			out.insert(host);
		}
		self.report.counts.domains += out.len();
		out.into_iter().collect()
	}

	/// Adds an application. Returns its name in the document.
	pub fn app(&mut self, at: &Place, spec: AppSpec) -> Option<String> {
		let AppSpec {
			name,
			mut app,
			vars,
			credentials,
		} = spec;
		let key = unique(&slugify(&name), |n| {
			self.doc
				.projects
				.get(&at.project)
				.and_then(|p| p.environments.get(&at.env))
				.is_some_and(|e| e.apps.contains_key(n))
		});
		let shown = at.path(&name);
		app.slug = unique(&slugify(&name), |s| self.slugs.contains(s));
		self.slugs.insert(app.slug.clone());
		app.server.clone_from(&self.server);
		app.domains = {
			let wanted = std::mem::take(&mut app.domains);
			self.claim_hosts(&shown, wanted)
		};
		app.env = self.placeholders(at, &key, &shown, vars);
		for (field, value) in credentials {
			let secret = secret_name(&at.project, &at.env, &key, &format!("#{field}"));
			let placeholder = names::placeholder(&secret);
			self.secrets.insert(secret, value);
			match field {
				"git_token" => app.git_token = Some(placeholder),
				"registry_username" => app.registry_username = Some(placeholder),
				"registry_password" => app.registry_password = Some(placeholder),
				_ => {}
			}
		}
		app.volumes.sort();
		app.volumes.dedup();
		self.report.counts.volumes += app.volumes.len();
		self.report.counts.apps += 1;
		let env = self.env_mut(at)?;
		env.apps.insert(key.clone(), app);
		Some(key)
	}

	/// Adds a database.
	pub fn database(&mut self, at: &Place, name: &str, mut db: DatabaseDoc) -> Option<String> {
		db.server.clone_from(&self.server);
		let key = unique(&slugify(name), |n| {
			self.doc
				.projects
				.get(&at.project)
				.is_some_and(|p| p.environments.values().any(|e| e.databases.contains_key(n)))
		});
		self.report.counts.databases += 1;
		let env = self.env_mut(at)?;
		env.databases.insert(key.clone(), db);
		Some(key)
	}

	/// Adds a compose stack.
	pub fn stack(&mut self, at: &Place, spec: StackSpec) -> Option<String> {
		let StackSpec {
			name,
			mut stack,
			vars,
		} = spec;
		let key = unique(&slugify(&name), |n| {
			self.doc
				.projects
				.get(&at.project)
				.is_some_and(|p| p.environments.values().any(|e| e.stacks.contains_key(n)))
		});
		let shown = at.path(&name);
		stack.server.clone_from(&self.server);
		stack.env = self.placeholders(at, &key, &shown, vars);
		let mut domains = BTreeMap::new();
		for (service, mut domain) in std::mem::take(&mut stack.domains) {
			let mut hosts = self.claim_hosts(&shown, vec![domain.host.clone()]);
			if let Some(host) = hosts.pop() {
				domain.host = host;
				domains.insert(service, domain);
			}
		}
		stack.domains = domains;
		self.report.counts.stacks += 1;
		let env = self.env_mut(at)?;
		env.stacks.insert(key.clone(), stack);
		Some(key)
	}

	/// Drops what turned out empty and hands over the result.
	#[must_use]
	pub fn finish(mut self) -> Imported {
		self.doc.projects.retain(|_, p| {
			p.environments.retain(|_, e| {
				!(e.apps.is_empty() && e.databases.is_empty() && e.stacks.is_empty())
			});
			!p.environments.is_empty()
		});
		self.report.counts.projects = self.doc.projects.len();
		self.report.counts.environments = self
			.doc
			.projects
			.values()
			.map(|p| p.environments.len())
			.sum();
		Imported {
			source: self.source,
			doc: self.doc,
			secrets: Secrets(self.secrets),
			report: self.report,
		}
	}
}

// ----- shared small readers

/// A string field that is present and not blank.
#[must_use]
pub fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
	v.get(key)
		.and_then(Value::as_str)
		.map(str::trim)
		.filter(|s| !s.is_empty())
}

/// A boolean field; `false` when missing.
#[must_use]
pub fn flag(v: &Value, key: &str) -> bool {
	v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// An array field.
#[must_use]
pub fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
	v.get(key)
		.and_then(Value::as_array)
		.map_or(&[], Vec::as_slice)
}

/// A port from a number or a string of digits.
#[must_use]
pub fn port_of(v: &Value) -> Option<i32> {
	let n = match v {
		Value::Number(n) => n.as_i64()?,
		Value::String(s) => s.trim().parse().ok()?,
		_ => return None,
	};
	i32::try_from(n).ok().filter(|p| (1..=65535).contains(p))
}

/// A memory size as Docker writes it (`512m`, `1g`, `2048`, bytes when bare) in megabytes.
/// Zero is "no limit", which is none.
#[must_use]
pub fn megabytes(text: &str) -> Option<i32> {
	let t = text.trim().to_ascii_lowercase();
	let (digits, unit) = t
		.find(|c: char| !c.is_ascii_digit() && c != '.')
		.map_or((t.as_str(), ""), |i| (&t[..i], &t[i..]));
	let n: f64 = digits.parse().ok()?;
	let bytes = match unit.trim_end_matches('b') {
		"" => n,
		"k" => n * 1024.0,
		"m" => n * 1024.0 * 1024.0,
		"g" => n * 1024.0 * 1024.0 * 1024.0,
		_ => return None,
	};
	#[allow(
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "bounded to a sane memory size just below"
	)]
	let mb = (bytes / (1024.0 * 1024.0)).round().clamp(0.0, 4_194_304.0) as i32;
	(mb > 0).then_some(mb)
}

/// An image reference split into the engine it runs and its version, for the managed
/// databases podkit has.
#[must_use]
pub fn engine_of(image: &str) -> Option<(&'static str, Option<String>)> {
	let name = image.rsplit('/').next().unwrap_or(image);
	let (base, tag) = name.split_once(':').unwrap_or((name, ""));
	let engine = match base {
		"postgres" | "postgresql" | "pgvector" => "postgres",
		"mysql" => "mysql",
		"mongo" | "mongodb" => "mongodb",
		"redis" => "redis",
		_ => return None,
	};
	let version = (!tag.is_empty() && tag != "latest").then(|| tag.to_owned());
	Some((engine, version))
}

/// The version of `engine` that podkit offers and the source's `tag` names: the tag's leading
/// number (`16-alpine` is `16`, `8.0.36` is `8.0`), when podkit has exactly that. `None`
/// means the engine's default; the caller says so.
#[must_use]
pub fn supported_version(engine: &str, tag: &str) -> Option<String> {
	use proto::Engine;
	let engine = match engine {
		"postgres" => Engine::Postgres,
		"mysql" => Engine::Mysql,
		"mongodb" => Engine::Mongodb,
		"redis" => Engine::Redis,
		_ => return None,
	};
	let numeric: String = tag
		.chars()
		.take_while(|c| c.is_ascii_digit() || *c == '.')
		.collect();
	let parts: Vec<&str> = numeric.split('.').filter(|p| !p.is_empty()).collect();
	let candidates = [
		parts.iter().take(2).copied().collect::<Vec<_>>().join("."),
		parts.first().map(|p| (*p).to_owned()).unwrap_or_default(),
	];
	let offered = crate::databases::engines::spec(engine).versions;
	candidates
		.into_iter()
		.find(|c| offered.contains(&c.as_str()))
}

/// A managed database for an engine image of the source. `None` when podkit has no such
/// engine (MariaDB, ClickHouse, Dragonfly, KeyDB, SQLite …).
pub fn managed_database(
	builder: &mut Builder,
	shown_at: &str,
	image: &str,
	public_port: Option<i32>,
) -> Option<DatabaseDoc> {
	let (engine, tag) = engine_of(image)?;
	let version = tag.as_deref().and_then(|t| supported_version(engine, t));
	if let Some(tag) = &tag
		&& version.is_none()
	{
		builder.check(
			shown_at,
			format!(
				"runs {engine} {tag}; podkit offers other versions of {engine} only, so the default one is used"
			),
		);
	}
	Some(DatabaseDoc {
		server: String::new(),
		engine: engine.to_owned(),
		version,
		public_port,
		memory_mb: None,
		cpu: None,
	})
}

impl Builder {
	/// Adds a scheduled task of an application of `at`.
	pub fn cron(&mut self, at: &Place, name: &str, task: crate::iac::doc::CronDoc) {
		let key = unique(&slugify(name), |n| {
			self.doc
				.projects
				.get(&at.project)
				.and_then(|p| p.environments.get(&at.env))
				.is_some_and(|e| e.cron.contains_key(n))
		});
		if let Some(env) = self.env_mut(at) {
			env.cron.insert(key, task);
		}
	}
}

/// A repository as podkit takes it (`https://`, `ssh://` or `user@host:path`). Says what it
/// had to assume.
///
/// `None` for an address that cannot be cloned by podkit at all.
pub fn repo_url(builder: &mut Builder, shown_at: &str, raw: &str) -> Option<String> {
	let raw = raw.trim().trim_end_matches('/');
	if raw.is_empty() {
		return None;
	}
	let url = if let Some(rest) = raw.strip_prefix("http://") {
		builder.check(
			shown_at,
			"the repository was on plain http; it is cloned over https here",
		);
		format!("https://{rest}")
	} else if raw.contains("://") || (raw.contains('@') && raw.contains(':')) {
		raw.to_owned()
	} else if raw.split('/').next().is_some_and(|h| h.contains('.')) && raw.contains('/') {
		// `github.com/owner/name`, as CapRover writes it.
		format!("https://{raw}")
	} else if raw.matches('/').count() == 1 && !raw.starts_with('/') {
		builder.check(
			shown_at,
			format!("the repository is \"{raw}\": taken to be on github.com, check the address"),
		);
		format!("https://github.com/{raw}")
	} else {
		return None;
	};
	crate::validate::repo_url(&url).then_some(url)
}
