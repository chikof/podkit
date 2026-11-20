//! The `podkit.toml` document: what a project looks like as a file.
//!
//! Every table is keyed by a name, never an id, so the same file means the same thing in any
//! team. Servers, SSH keys and registries are named, not described: they must exist where the
//! file is applied. Secrets are never part of a document podkit writes: each one is the
//! placeholder `${secret:NAME}`, filled in from a separate secrets file when the document is
//! applied.
//!
//! Maps are [`BTreeMap`]s and lists are sorted on export, so the same project always
//! produces the same bytes and a `git diff` shows only what changed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only version this build reads and writes.
pub const VERSION: u32 = 1;

/// A whole file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Doc {
	pub version: u32,
	/// By project slug. An export holds one project; an import may hold several.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub projects: BTreeMap<String, ProjectDoc>,
}

impl Doc {
	#[must_use]
	pub fn new() -> Self {
		Self {
			version: VERSION,
			projects: BTreeMap::new(),
		}
	}
}

impl Default for Doc {
	fn default() -> Self {
		Self::new()
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDoc {
	pub name: String,
	/// By environment slug.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub environments: BTreeMap<String, EnvironmentDoc>,
	/// User networks with a member in this project, ordered by server and name.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub networks: Vec<NetworkDoc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDoc {
	pub name: String,
	/// By application name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub apps: BTreeMap<String, AppDoc>,
	/// By database name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub databases: BTreeMap<String, DatabaseDoc>,
	/// By stack name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub stacks: BTreeMap<String, StackDoc>,
	/// By model name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub models: BTreeMap<String, ModelDoc>,
	/// By task name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub cron: BTreeMap<String, CronDoc>,
}

/// A scheduled task. Defaults are left out of an export: `UTC`, 300 seconds, `skip`, no
/// retries, 60 seconds before the first retry, enabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronDoc {
	/// `exec` (inside an application's running container) or `container` (a one-shot one).
	pub kind: String,
	/// The name of an application of the same environment: where an `exec` task runs, and
	/// what a `container` task inherits its environment and networks from.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub app: Option<String>,
	/// The server a `container` task without an `app` runs on.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub server: Option<String>,
	/// A `container` task's image; its application's when omitted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub image: Option<String>,
	pub command: String,
	/// Five cron fields.
	pub schedule: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timezone: Option<String>,
	/// Seconds before the run is killed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<i32>,
	/// `skip`, `queue` or `allow`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub concurrency: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub retries: Option<i32>,
	/// Seconds before the first retry; it doubles each time.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub retry_delay: Option<i32>,
	/// `false` pauses the task.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub enabled: Option<bool>,
}

/// An application: a git repository that is built, or an image that is pulled. Exactly one of
/// `repo` and `image` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppDoc {
	/// The name of a server of the team.
	pub server: String,
	/// The public subdomain, unique on its server.
	pub slug: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub repo: Option<String>,
	#[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
	pub git_ref: Option<String>,
	/// `dockerfile` (the default) or `nixpacks`. Git applications only.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub build: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub dockerfile: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub image: Option<String>,
	pub port: i32,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub memory_mb: Option<i32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cpu: Option<f64>,
	/// The name of an SSH key of the team, used to clone an `ssh://` repository.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub deploy_key: Option<String>,
	/// The name of a saved registry the image is pulled with.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub registry: Option<String>,
	/// Secret: a token for an `https://` repository.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub git_token: Option<String>,
	/// Secret: a registry login that belongs to this application alone.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub registry_username: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub registry_password: Option<String>,
	/// Custom hostnames, sorted.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub domains: Vec<String>,
	/// Mount paths of persistent volumes, sorted.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub volumes: Vec<String>,
	/// Environment variables: a `${secret:NAME}` placeholder each when exported.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub env: BTreeMap<String, String>,
	/// Models of the same project the application is linked to, by model name.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub models: Vec<ModelLinkDoc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLinkDoc {
	pub model: String,
	/// The model `PODKIT_MODEL` names.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub default: bool,
}

/// A managed database. Its credentials are generated by podkit and are not part of the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseDoc {
	pub server: String,
	/// `postgres`, `mysql`, `mongodb` or `redis`.
	pub engine: String,
	/// The engine's default version when omitted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub version: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub public_port: Option<i32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub memory_mb: Option<i32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cpu: Option<f64>,
}

/// A compose stack, with the compose file inline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackDoc {
	pub server: String,
	pub compose: String,
	/// Where it came from: `raw`, or `template:<id>@<version>`. Informational.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
	/// The name of a saved registry the images are pulled with.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub registry: Option<String>,
	/// Variables of the stack: a `${secret:NAME}` placeholder each when exported.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub env: BTreeMap<String, String>,
	/// Files a template mounts, by path: a placeholder each when exported.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub files: BTreeMap<String, String>,
	/// Hostnames routed to a service, by service name.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub domains: BTreeMap<String, StackDomainDoc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackDomainDoc {
	pub host: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub port: Option<u16>,
}

/// A model: an inference engine serving downloaded files. The files themselves are
/// downloaded separately and referenced by repository and revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDoc {
	pub server: String,
	pub repo: String,
	pub revision: String,
	pub engine: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub image: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub port: Option<u16>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub model_path: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub served_name: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub context_size: Option<u32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub gpu: Option<bool>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub extra_args: Vec<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub memory_mb: Option<i32>,
}

/// A user network of a server and the resources of this project on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDoc {
	pub name: String,
	pub server: String,
	/// No outbound access.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub internal: bool,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub members: Vec<MemberDoc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDoc {
	/// The slug of the member's environment.
	pub environment: String,
	/// `application`, `database`, `model` or `stack`.
	pub kind: String,
	pub name: String,
	/// The names it answers to on the network; its own name when empty.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub aliases: Vec<String>,
}

/// Why a file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
	#[error("{0}")]
	Parse(String),
	#[error("podkit.toml version {0} is not supported; this podkit reads version {VERSION}")]
	Version(u32),
}

/// Reads a document.
///
/// # Errors
/// [`DocError`] for text that is not TOML, has keys podkit does not know, or has another
/// `version`.
pub fn parse(text: &str) -> Result<Doc, DocError> {
	let doc: Doc = toml::from_str(text).map_err(|e| DocError::Parse(e.to_string()))?;
	if doc.version != VERSION {
		return Err(DocError::Version(doc.version));
	}
	Ok(doc)
}

/// Writes a document as TOML. Deterministic: the same document is always the same text.
///
/// # Errors
/// [`DocError::Parse`] when the document cannot be written (it holds nothing that cannot).
pub fn render(doc: &Doc) -> Result<String, DocError> {
	let mut text = toml::to_string_pretty(doc).map_err(|e| DocError::Parse(e.to_string()))?;
	if !text.ends_with('\n') {
		text.push('\n');
	}
	Ok(format!(
		"# podkit.toml: generated by podkit; secrets are `${{secret:NAME}}` placeholders, never values.\n{text}"
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> Doc {
		let mut doc = Doc::new();
		let mut env = BTreeMap::new();
		env.insert("B".to_owned(), "${secret:prod/web/B}".to_owned());
		env.insert("A".to_owned(), "${secret:prod/web/A}".to_owned());
		let app = AppDoc {
			server: "main".into(),
			slug: "web".into(),
			repo: Some("https://example.com/r.git".into()),
			git_ref: Some("main".into()),
			build: Some("dockerfile".into()),
			dockerfile: Some("Dockerfile".into()),
			image: None,
			port: 8080,
			memory_mb: None,
			cpu: None,
			deploy_key: None,
			registry: None,
			git_token: None,
			registry_username: None,
			registry_password: None,
			domains: vec!["a.example.com".into()],
			volumes: vec![],
			env,
			models: vec![],
		};
		let mut environment = EnvironmentDoc {
			name: "Production".into(),
			apps: BTreeMap::new(),
			databases: BTreeMap::new(),
			stacks: BTreeMap::new(),
			models: BTreeMap::new(),
			cron: BTreeMap::new(),
		};
		environment.apps.insert("web".into(), app);
		environment.stacks.insert(
			"cache".into(),
			StackDoc {
				server: "main".into(),
				compose: "services:\n  redis:\n    image: redis:7\n".into(),
				source: Some("raw".into()),
				registry: None,
				env: BTreeMap::new(),
				files: BTreeMap::new(),
				domains: BTreeMap::new(),
			},
		);
		let mut project = ProjectDoc {
			name: "Shop".into(),
			environments: BTreeMap::new(),
			networks: vec![NetworkDoc {
				name: "back".into(),
				server: "main".into(),
				internal: true,
				members: vec![MemberDoc {
					environment: "production".into(),
					kind: "application".into(),
					name: "web".into(),
					aliases: vec![],
				}],
			}],
		};
		project
			.environments
			.insert("production".into(), environment);
		doc.projects.insert("shop".into(), project);
		doc
	}

	#[test]
	fn a_document_round_trips_through_text_and_is_stable() {
		let doc = sample();
		let text = render(&doc).unwrap();
		assert_eq!(parse(&text).unwrap(), doc, "{text}");
		assert_eq!(render(&parse(&text).unwrap()).unwrap(), text);
		assert!(
			text.find("A = ").unwrap() < text.find("B = ").unwrap(),
			"keys come out sorted:\n{text}"
		);
	}

	#[test]
	fn unknown_keys_and_other_versions_are_refused() {
		let err = parse("version = 1\nsurprise = true\n").unwrap_err();
		assert!(err.to_string().contains("surprise"), "{err}");
		assert!(matches!(parse("version = 2\n"), Err(DocError::Version(2))));
		assert!(parse("version = [").is_err());
	}
}
