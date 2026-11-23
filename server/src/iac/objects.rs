//! A document, flattened to comparable objects.
//!
//! The same flattening reads the project as it is (the export) and the file the user wrote, so
//! a difference between the two is a difference the user can see in the file. Every object is
//! a map of dotted field names to values; collections (an application's variables, domains,
//! volumes and links, a network's members) are one field per entry, so a diff names the entry
//! that changed.
//!
//! Secrets compare by digest and are never kept as text past this module: a field holds the
//! SHA-256 of the value, and a placeholder the caller gave no value for is
//! [`Fv::Unresolved`], which stands for "whatever is stored".

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::doc::{
	AppDoc, CronDoc, DatabaseDoc, EnvironmentDoc, ModelDoc, NetworkDoc, ProjectDoc, StackDoc,
};
use crate::cron::limits::{DEFAULT_BACKOFF_S, DEFAULT_TIMEOUT_S};

/// The kinds of object a file describes, in the order they are applied: what the others
/// need comes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
	Environment,
	Network,
	Database,
	Model,
	Stack,
	App,
	/// A scheduled task: it names an application of its environment, so it comes after.
	Cron,
}

impl Kind {
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Environment => "environment",
			Self::Network => "network",
			Self::Database => "database",
			Self::Model => "model",
			Self::Stack => "stack",
			Self::App => "app",
			Self::Cron => "cron",
		}
	}

	/// Whether the object runs a container that a change replaces.
	#[must_use]
	pub fn runs(self) -> bool {
		!matches!(self, Self::Environment | Self::Network | Self::Cron)
	}
}

/// What names an object: its kind, the slug of its environment (none for a network) and its
/// name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
	pub kind: Kind,
	pub environment: Option<String>,
	pub name: String,
}

impl Key {
	#[must_use]
	pub fn of(kind: Kind, environment: Option<&str>, name: &str) -> Self {
		Self {
			kind,
			environment: environment.map(str::to_owned),
			name: name.to_owned(),
		}
	}

	/// `production/web`, as the object is named in a message.
	#[must_use]
	pub fn path(&self) -> String {
		match &self.environment {
			Some(env) => format!("{env}/{}", self.name),
			None => self.name.clone(),
		}
	}
}

/// A field's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Fv {
	Val(Value),
	/// The SHA-256 of a secret.
	Secret(String),
	/// A `${secret:NAME}` the caller supplied no value for.
	Unresolved(String),
}

impl Fv {
	/// How a change shows it: a secret never appears.
	#[must_use]
	pub fn shown(&self) -> Value {
		match self {
			Self::Val(v) => v.clone(),
			Self::Secret(_) => json!("(secret)"),
			Self::Unresolved(name) => json!(format!("${{secret:{name}}}")),
		}
	}

	/// Equality as the diff means it: an unresolved placeholder matches any stored secret.
	#[must_use]
	pub fn matches(&self, stored: &Self) -> bool {
		match (self, stored) {
			(Self::Unresolved(_), Self::Secret(_)) | (Self::Secret(_), Self::Unresolved(_)) => true,
			_ => self == stored,
		}
	}
}

#[derive(Debug, Clone)]
pub struct Obj {
	pub key: Key,
	pub fields: BTreeMap<String, Fv>,
}

/// Field-name prefixes of collections: an entry missing from the file is a delete, not an
/// update to nothing.
pub const COLLECTIONS: [&str; 6] = ["env.", "domain.", "volume.", "link.", "member.", "file."];

#[must_use]
pub fn is_collection(field: &str) -> bool {
	COLLECTIONS.iter().any(|p| field.starts_with(p))
}

/// Write-only singletons: stored, never read back, so a file that leaves one out leaves it be.
/// A deploy key is one too: a file can pick another key but cannot take the key away.
#[must_use]
pub fn is_write_only(field: &str) -> bool {
	matches!(
		field,
		"git_token" | "registry_username" | "registry_password" | "deploy_key"
	)
}

/// The values behind `${secret:NAME}` placeholders.
#[derive(Debug, Clone, Default)]
pub struct Secrets(pub BTreeMap<String, String>);

/// A string with its placeholders filled in.
pub enum Resolved {
	Known(String),
	Missing(String),
}

impl Secrets {
	/// Reads a secrets file: a flat TOML table of `"NAME" = "value"`.
	///
	/// # Errors
	/// A message for text that is not such a table.
	pub fn parse(text: &str) -> Result<Self, String> {
		let table: BTreeMap<String, String> =
			toml::from_str(text).map_err(|e| format!("the secrets file is not valid: {e}"))?;
		Ok(Self(table))
	}

	/// Fills in every `${secret:NAME}` of `raw`; plain text is its own value.
	#[must_use]
	pub fn resolve(&self, raw: &str) -> Resolved {
		let mut out = String::with_capacity(raw.len());
		let mut rest = raw;
		while let Some(start) = rest.find("${secret:") {
			out.push_str(&rest[..start]);
			let after = &rest[start + "${secret:".len()..];
			let Some(end) = after.find('}') else {
				out.push_str(&rest[start..]);
				return Resolved::Known(out);
			};
			let name = &after[..end];
			match self.0.get(name) {
				Some(value) => out.push_str(value),
				None => return Resolved::Missing(name.to_owned()),
			}
			rest = &after[end + 1..];
		}
		out.push_str(rest);
		Resolved::Known(out)
	}
}

/// The SHA-256 of a secret, in hex.
#[must_use]
pub fn digest(value: &str) -> String {
	use std::fmt::Write as _;
	let mut hasher = Sha256::new();
	hasher.update(value.as_bytes());
	hasher.finalize().iter().fold(String::new(), |mut s, b| {
		let _ = write!(s, "{b:02x}");
		s
	})
}

/// The names the placeholders of an exported secret get.
pub mod names {
	#[must_use]
	pub fn env_var(environment: &str, resource: &str, key: &str) -> String {
		format!("{environment}/{resource}/{key}")
	}

	#[must_use]
	pub fn singleton(environment: &str, resource: &str, field: &str) -> String {
		format!("{environment}/{resource}#{field}")
	}

	#[must_use]
	pub fn file(environment: &str, stack: &str, path: &str) -> String {
		format!("{environment}/{stack}#file:{path}")
	}

	#[must_use]
	pub fn placeholder(name: &str) -> String {
		format!("${{secret:{name}}}")
	}
}

struct Flat<'a> {
	secrets: &'a Secrets,
	out: Vec<Obj>,
}

impl Flat<'_> {
	fn secret(&self, raw: &str) -> Fv {
		match self.secrets.resolve(raw) {
			Resolved::Known(v) => Fv::Secret(digest(&v)),
			Resolved::Missing(name) => Fv::Unresolved(name),
		}
	}

	fn push(&mut self, key: Key, fields: BTreeMap<String, Fv>) {
		self.out.push(Obj { key, fields });
	}
}

fn val(fields: &mut BTreeMap<String, Fv>, name: &str, v: impl Into<Value>) {
	fields.insert(name.to_owned(), Fv::Val(v.into()));
}

fn opt<T: Into<Value>>(fields: &mut BTreeMap<String, Fv>, name: &str, v: Option<T>) {
	if let Some(v) = v {
		val(fields, name, v);
	}
}

/// Compose text as compared: Unix line ends, no trailing blank space.
#[must_use]
pub fn normal_text(text: &str) -> String {
	text.replace("\r\n", "\n").trim_end().to_owned()
}

/// What a git application gets when the file says nothing.
pub const DEFAULT_BUILD: &str = "dockerfile";
pub const DEFAULT_DOCKERFILE: &str = "Dockerfile";
pub const DEFAULT_REF: &str = "main";

/// An f64 as a 32-bit float stores it, so what the database holds equals what a file says.
#[must_use]
pub fn as_cpu(cpu: f64) -> f64 {
	#[allow(
		clippy::cast_possible_truncation,
		reason = "the column is a 32-bit float"
	)]
	let narrow = cpu as f32;
	format!("{narrow}").parse().unwrap_or(cpu)
}

fn app_fields(flat: &Flat<'_>, app: &AppDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "server", app.server.as_str());
	val(&mut f, "slug", app.slug.as_str());
	let git = app.image.is_none();
	val(&mut f, "source", if git { "git" } else { "image" });
	if git {
		opt(&mut f, "repo", app.repo.as_deref());
		val(&mut f, "ref", app.git_ref.as_deref().unwrap_or(DEFAULT_REF));
		val(
			&mut f,
			"build",
			app.build.as_deref().unwrap_or(DEFAULT_BUILD),
		);
		val(
			&mut f,
			"dockerfile",
			app.dockerfile.as_deref().unwrap_or(DEFAULT_DOCKERFILE),
		);
	} else {
		opt(&mut f, "image", app.image.as_deref());
	}
	val(&mut f, "port", app.port);
	opt(&mut f, "memory_mb", app.memory_mb);
	opt(&mut f, "cpu", app.cpu.map(as_cpu));
	opt(&mut f, "deploy_key", app.deploy_key.as_deref());
	opt(&mut f, "registry", app.registry.as_deref());
	for (name, value) in [
		("git_token", &app.git_token),
		("registry_username", &app.registry_username),
		("registry_password", &app.registry_password),
	] {
		if let Some(raw) = value {
			f.insert(name.to_owned(), flat.secret(raw));
		}
	}
	for (key, raw) in &app.env {
		f.insert(format!("env.{key}"), flat.secret(raw));
	}
	for host in &app.domains {
		val(
			&mut f,
			&format!("domain.{}", host.to_ascii_lowercase()),
			true,
		);
	}
	for path in &app.volumes {
		val(&mut f, &format!("volume.{path}"), true);
	}
	for link in &app.models {
		val(&mut f, &format!("link.{}", link.model), link.default);
	}
	f
}

fn database_fields(db: &DatabaseDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "server", db.server.as_str());
	val(&mut f, "engine", db.engine.as_str());
	opt(&mut f, "version", db.version.as_deref());
	opt(&mut f, "public_port", db.public_port);
	opt(&mut f, "memory_mb", db.memory_mb);
	opt(&mut f, "cpu", db.cpu.map(as_cpu));
	f
}

fn stack_fields(flat: &Flat<'_>, stack: &StackDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "server", stack.server.as_str());
	val(&mut f, "compose", normal_text(&stack.compose));
	opt(&mut f, "registry", stack.registry.as_deref());
	for (key, raw) in &stack.env {
		f.insert(format!("env.{key}"), flat.secret(raw));
	}
	for (path, raw) in &stack.files {
		f.insert(format!("file.{path}"), flat.secret(raw));
	}
	for (service, domain) in &stack.domains {
		let host = domain.host.to_ascii_lowercase();
		let shown = match domain.port {
			Some(port) => format!("{host}:{port}"),
			None => host,
		};
		val(&mut f, &format!("domain.{service}"), shown);
	}
	f
}

fn model_fields(model: &ModelDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "server", model.server.as_str());
	val(&mut f, "repo", model.repo.as_str());
	val(&mut f, "revision", model.revision.as_str());
	val(&mut f, "engine", model.engine.as_str());
	opt(&mut f, "image", model.image.as_deref());
	opt(&mut f, "port", model.port);
	opt(&mut f, "model_path", model.model_path.as_deref());
	opt(&mut f, "served_name", model.served_name.as_deref());
	opt(&mut f, "context_size", model.context_size);
	opt(&mut f, "gpu", model.gpu);
	if !model.extra_args.is_empty() {
		val(&mut f, "extra_args", json!(model.extra_args));
	}
	opt(&mut f, "memory_mb", model.memory_mb);
	f
}

/// What a scheduled task gets when the file says nothing.
pub const DEFAULT_ZONE: &str = "UTC";
pub const DEFAULT_CONCURRENCY: &str = "skip";

fn cron_fields(task: &CronDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "kind", task.kind.as_str());
	opt(&mut f, "app", task.app.as_deref());
	opt(&mut f, "server", task.server.as_deref());
	opt(
		&mut f,
		"image",
		task.image
			.as_deref()
			.map(str::trim)
			.filter(|i| !i.is_empty()),
	);
	val(&mut f, "command", task.command.trim());
	val(&mut f, "schedule", task.schedule.trim());
	val(
		&mut f,
		"timezone",
		task.timezone.as_deref().unwrap_or(DEFAULT_ZONE),
	);
	val(&mut f, "timeout", task.timeout.unwrap_or(DEFAULT_TIMEOUT_S));
	val(
		&mut f,
		"concurrency",
		task.concurrency.as_deref().unwrap_or(DEFAULT_CONCURRENCY),
	);
	val(&mut f, "retries", task.retries.unwrap_or(0));
	val(
		&mut f,
		"retry_delay",
		task.retry_delay.unwrap_or(DEFAULT_BACKOFF_S),
	);
	val(&mut f, "enabled", task.enabled.unwrap_or(true));
	f
}

/// The alias a member answers to when none is given.
#[must_use]
pub fn default_alias(name: &str) -> String {
	crate::networks::slugify(name).unwrap_or_else(|| name.to_owned())
}

fn network_fields(network: &NetworkDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "server", network.server.as_str());
	val(&mut f, "internal", network.internal);
	for member in &network.members {
		let aliases = if member.aliases.is_empty() {
			vec![default_alias(&member.name)]
		} else {
			let mut a = member.aliases.clone();
			a.sort();
			a
		};
		val(
			&mut f,
			&format!(
				"member.{}/{}/{}",
				member.environment, member.kind, member.name
			),
			json!(aliases),
		);
	}
	f
}

fn environment_fields(env: &EnvironmentDoc) -> BTreeMap<String, Fv> {
	let mut f = BTreeMap::new();
	val(&mut f, "name", env.name.as_str());
	f
}

/// Every object of a project, environments first.
#[must_use]
pub fn flatten(project: &ProjectDoc, secrets: &Secrets) -> Vec<Obj> {
	let mut flat = Flat {
		secrets,
		out: Vec::new(),
	};
	for (slug, env) in &project.environments {
		flat.push(
			Key::of(Kind::Environment, Some(slug), slug),
			environment_fields(env),
		);
		for (name, app) in &env.apps {
			let fields = app_fields(&flat, app);
			flat.push(Key::of(Kind::App, Some(slug), name), fields);
		}
		for (name, db) in &env.databases {
			flat.push(
				Key::of(Kind::Database, Some(slug), name),
				database_fields(db),
			);
		}
		for (name, stack) in &env.stacks {
			let fields = stack_fields(&flat, stack);
			flat.push(Key::of(Kind::Stack, Some(slug), name), fields);
		}
		for (name, model) in &env.models {
			flat.push(Key::of(Kind::Model, Some(slug), name), model_fields(model));
		}
		for (name, task) in &env.cron {
			flat.push(Key::of(Kind::Cron, Some(slug), name), cron_fields(task));
		}
	}
	for network in &project.networks {
		let name = format!("{}/{}", network.server, network.name);
		flat.push(Key::of(Kind::Network, None, &name), network_fields(network));
	}
	flat.out.sort_by(|a, b| a.key.cmp(&b.key));
	flat.out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn placeholders_are_filled_in_and_a_missing_one_is_named() {
		let secrets = Secrets(BTreeMap::from([("a/b".to_owned(), "s3".to_owned())]));
		assert!(matches!(secrets.resolve("plain"), Resolved::Known(v) if v == "plain"));
		assert!(
			matches!(secrets.resolve("x:${secret:a/b}@h"), Resolved::Known(v) if v == "x:s3@h")
		);
		assert!(matches!(secrets.resolve("${secret:nope}"), Resolved::Missing(n) if n == "nope"));
	}

	#[test]
	fn a_secrets_file_is_a_flat_table() {
		let s = Secrets::parse("\"prod/web/KEY\" = \"v\"\nOTHER = \"w\"\n").unwrap();
		assert_eq!(s.0.get("prod/web/KEY").map(String::as_str), Some("v"));
		assert!(Secrets::parse("[nested]\na = 1\n").is_err());
	}

	#[test]
	fn secrets_show_as_a_mask_and_unresolved_matches_any_stored_secret() {
		assert_eq!(Fv::Secret("abc".into()).shown(), json!("(secret)"));
		assert!(Fv::Unresolved("x".into()).matches(&Fv::Secret("abc".into())));
		assert!(!Fv::Secret("a".into()).matches(&Fv::Secret("b".into())));
		assert!(Fv::Secret("a".into()).matches(&Fv::Secret("a".into())));
	}

	#[test]
	fn cpu_compares_as_the_database_stores_it() {
		assert!((as_cpu(0.1) - 0.1).abs() < 1e-9);
		assert!((as_cpu(0.5) - 0.5).abs() < 1e-9);
	}
}
