//! Reads a project as it is: the document an export writes, the values behind its secret
//! placeholders (kept in memory, for comparing), and the ids an apply needs.
//!
//! The caller has authorized the reads; this module only asks the database.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::doc::{
	AppDoc, DatabaseDoc, EnvironmentDoc, MemberDoc, ModelDoc, ModelLinkDoc, NetworkDoc, ProjectDoc,
	StackDoc, StackDomainDoc,
};
use super::objects::{Key, Kind, Secrets, as_cpu, default_alias, names};
use super::plan::Refs;
use crate::applications::{self, SourceKind, StoredRegistryAuth};
use crate::error::{AppResult, ServerError};
use crate::ids::{ModelFileId, RegistryId, ServerId, SshKeyId, TeamId};
use crate::networks::ResourceKind;
use crate::projects::Project;
use crate::state::AppState;
use crate::{
	databases, domains, env_vars, environments, links, model_files, models, networks, stacks,
	volumes,
};

/// What a team has that a file names: servers, SSH keys, registries and model downloads, by
/// name, with their ids.
#[derive(Debug, Clone, Default)]
pub struct Lookup {
	pub servers: BTreeMap<String, ServerId>,
	pub ssh_keys: BTreeMap<String, SshKeyId>,
	pub registries: BTreeMap<String, RegistryId>,
	pub model_files: BTreeMap<(String, String, String), ModelFileId>,
	server_names: HashMap<ServerId, String>,
	key_names: HashMap<SshKeyId, String>,
	registry_names: HashMap<RegistryId, String>,
	file_refs: HashMap<ModelFileId, (String, String, String)>,
}

impl Lookup {
	/// # Errors
	/// `Forbidden` for a caller who may not read servers, SSH keys or registries.
	pub async fn load(state: &AppState, user: crate::ids::UserId, team: TeamId) -> AppResult<Self> {
		let mut me = Self::default();
		for server in crate::servers::service::list(&state.db, user, team).await? {
			me.server_names.insert(server.id, server.name.clone());
			me.servers.insert(server.name, server.id);
		}
		for key in crate::ssh_keys::service::list(&state.db, user, team).await? {
			me.key_names.insert(key.id, key.name.clone());
			me.ssh_keys.insert(key.name, key.id);
		}
		for registry in crate::registries::service::list(&state.db, user, team).await? {
			me.registry_names.insert(registry.id, registry.name.clone());
			me.registries.insert(registry.name, registry.id);
		}
		for file in model_files::list(&state.db, team).await? {
			let Some(server) = me.server_names.get(&file.server_id) else {
				continue;
			};
			let key = (server.clone(), file.repo.clone(), file.revision.clone());
			me.file_refs.insert(file.id, key.clone());
			me.model_files.entry(key).or_insert(file.id);
		}
		Ok(me)
	}

	#[must_use]
	pub fn refs(&self) -> Refs {
		Refs {
			servers: self.servers.keys().cloned().collect(),
			ssh_keys: self.ssh_keys.keys().cloned().collect(),
			registries: self.registries.keys().cloned().collect(),
			model_files: self.model_files.keys().cloned().collect(),
		}
	}

	fn server(&self, id: ServerId) -> String {
		self.server_names.get(&id).cloned().unwrap_or_default()
	}
}

/// Ids of what exists, by the key a plan names it with.
#[derive(Debug, Clone, Default)]
pub struct Ids {
	pub environments: BTreeMap<String, crate::ids::EnvironmentId>,
	pub objects: BTreeMap<Key, i64>,
	/// Model name to id, project-wide: a link names a model without its environment.
	pub models: BTreeMap<String, crate::ids::ModelId>,
	/// Network name and server to id, as `server/name`.
	pub networks: BTreeMap<String, crate::ids::NetworkId>,
}

pub struct Snapshot {
	pub doc: ProjectDoc,
	/// The values behind the placeholders of `doc`. Never leaves the process.
	pub secrets: Secrets,
	pub ids: Ids,
}

fn internal(what: &str, e: impl std::fmt::Display) -> ServerError {
	tracing::error!("iac export: {what}: {e}");
	ServerError::Internal
}

fn open(state: &AppState, blob: &[u8], what: &str) -> AppResult<String> {
	state.secrets.decrypt(blob).map_err(|e| internal(what, e))
}

fn wire<T: serde::Serialize>(value: T) -> String {
	match serde_json::to_value(value) {
		Ok(Value::String(s)) => s,
		_ => String::new(),
	}
}

/// A secret: its placeholder goes in the document, its value in `secrets`.
fn secret(secrets: &mut Secrets, name: String, value: String) -> String {
	let placeholder = names::placeholder(&name);
	secrets.0.insert(name, value);
	placeholder
}

fn cpu(value: Option<f32>) -> Option<f64> {
	value.map(|c| as_cpu(f64::from(c)))
}

/// Reads `project` of a team.
///
/// # Errors
/// `Internal` when a stored secret cannot be opened.
#[allow(
	clippy::too_many_lines,
	reason = "one pass over each kind of resource, kept together to read as the export order"
)]
pub async fn read(state: &AppState, project: &Project, lookup: &Lookup) -> AppResult<Snapshot> {
	let db = &state.db;
	let mut doc = ProjectDoc {
		name: project.name.clone(),
		environments: BTreeMap::new(),
		networks: Vec::new(),
	};
	let mut secrets = Secrets::default();
	let mut ids = Ids::default();
	// (resource kind, id) to (environment slug, name), for placing network members.
	let mut index: BTreeMap<(&'static str, i64), (String, String)> = BTreeMap::new();

	for env in environments::list_by_project(db, project.id).await? {
		let slug = env.slug.clone();
		ids.environments.insert(slug.clone(), env.id);
		ids.objects
			.insert(Key::of(Kind::Environment, Some(&slug), &slug), env.id.0);
		let mut out = EnvironmentDoc {
			name: env.name.clone(),
			apps: BTreeMap::new(),
			databases: BTreeMap::new(),
			stacks: BTreeMap::new(),
			models: BTreeMap::new(),
		};

		for app in applications::list_by_environment(db, env.id).await? {
			let git = app.source_kind == SourceKind::Git;
			let mut env_map = BTreeMap::new();
			for var in env_vars::list(db, app.id).await? {
				let value = open(state, &var.value, "env var")?;
				let name = names::env_var(&slug, &app.name, &var.key);
				env_map.insert(var.key, secret(&mut secrets, name, value));
			}
			let mut doc_app = AppDoc {
				server: lookup.server(app.server_id),
				slug: app.slug.clone(),
				repo: app.repo_url.clone().filter(|_| git),
				git_ref: git.then(|| app.git_ref.clone()),
				build: git.then(|| app.build_strategy.as_str().to_owned()),
				dockerfile: git.then(|| app.dockerfile_path.clone()),
				image: app.image.clone().filter(|_| !git),
				port: app.container_port,
				memory_mb: app.memory_limit_mb,
				cpu: cpu(app.cpu_limit),
				deploy_key: app
					.deploy_key_id
					.and_then(|id| lookup.key_names.get(&id).cloned()),
				registry: app
					.registry_id
					.and_then(|id| lookup.registry_names.get(&id).cloned()),
				git_token: None,
				registry_username: None,
				registry_password: None,
				domains: Vec::new(),
				volumes: Vec::new(),
				env: env_map,
				models: Vec::new(),
			};
			if let Some(blob) = &app.git_token {
				let value = open(state, blob, "git token")?;
				let name = names::singleton(&slug, &app.name, "git_token");
				doc_app.git_token = Some(secret(&mut secrets, name, value));
			}
			if let Some(blob) = &app.registry_auth {
				let stored: StoredRegistryAuth =
					serde_json::from_str(&open(state, blob, "registry login")?)
						.map_err(|e| internal("registry login", e))?;
				let user = names::singleton(&slug, &app.name, "registry_username");
				let pass = names::singleton(&slug, &app.name, "registry_password");
				doc_app.registry_username = Some(secret(&mut secrets, user, stored.username));
				doc_app.registry_password = Some(secret(&mut secrets, pass, stored.password));
			}
			let mut hosts: Vec<String> = domains::list(db, app.id)
				.await?
				.into_iter()
				.map(|d| d.hostname)
				.collect();
			hosts.sort();
			doc_app.domains = hosts;
			let mut mounts: Vec<String> = volumes::list(db, app.id)
				.await?
				.into_iter()
				.map(|v| v.mount_path)
				.collect();
			mounts.sort();
			doc_app.volumes = mounts;
			let mut linked: Vec<ModelLinkDoc> = links::list(db, app.id)
				.await?
				.into_iter()
				.map(|l| ModelLinkDoc {
					model: l.name,
					default: l.is_default,
				})
				.collect();
			linked.sort_by(|a, b| a.model.cmp(&b.model));
			doc_app.models = linked;
			ids.objects
				.insert(Key::of(Kind::App, Some(&slug), &app.name), app.id.0);
			index.insert(
				(ResourceKind::Application.as_str(), app.id.0),
				(slug.clone(), app.name.clone()),
			);
			out.apps.insert(app.name, doc_app);
		}

		for database in databases::list_by_environment(db, env.id).await? {
			ids.objects.insert(
				Key::of(Kind::Database, Some(&slug), &database.name),
				database.id.0,
			);
			index.insert(
				(ResourceKind::Database.as_str(), database.id.0),
				(slug.clone(), database.name.clone()),
			);
			out.databases.insert(
				database.name.clone(),
				DatabaseDoc {
					server: lookup.server(database.server_id),
					engine: wire(database.engine),
					version: Some(database.version.clone()),
					public_port: database.public_port,
					memory_mb: database.memory_limit_mb,
					cpu: cpu(database.cpu_limit),
				},
			);
		}

		for stack in stacks::list_by_environment(db, env.id).await? {
			let mut env_map = BTreeMap::new();
			for (key, value) in stacks::decrypt_env(&state.secrets, &stack)
				.map_err(|e| internal("stack variables", e))?
			{
				let name = names::env_var(&slug, &stack.name, &key);
				env_map.insert(key, secret(&mut secrets, name, value));
			}
			let mut files = BTreeMap::new();
			for (path, value) in stacks::decrypt_files(&state.secrets, &stack)
				.map_err(|e| internal("stack files", e))?
			{
				let name = names::file(&slug, &stack.name, &path);
				files.insert(path, secret(&mut secrets, name, value));
			}
			let mut domains_map = BTreeMap::new();
			for service in stacks::list_services(db, stack.id).await? {
				if let Some(host) = service.domain {
					domains_map.insert(
						service.name,
						StackDomainDoc {
							host,
							port: service.domain_port.and_then(|p| u16::try_from(p).ok()),
						},
					);
				}
			}
			ids.objects
				.insert(Key::of(Kind::Stack, Some(&slug), &stack.name), stack.id.0);
			index.insert(
				(ResourceKind::Stack.as_str(), stack.id.0),
				(slug.clone(), stack.name.clone()),
			);
			out.stacks.insert(
				stack.name.clone(),
				StackDoc {
					server: lookup.server(stack.server_id),
					compose: stack.compose_yaml.clone(),
					source: Some(stack.source.clone()).filter(|s| s != "raw"),
					registry: stack
						.registry_id
						.and_then(|id| lookup.registry_names.get(&id).cloned()),
					env: env_map,
					files,
					domains: domains_map,
				},
			);
		}

		for model in models::list_by_environment(db, env.id).await? {
			let (repo, revision) = lookup
				.file_refs
				.get(&model.model_files_id)
				.map(|(_, repo, revision)| (repo.clone(), revision.clone()))
				.unwrap_or_default();
			let custom = model.engine == models::EngineKind::Custom;
			ids.models.insert(model.name.clone(), model.id);
			ids.objects
				.insert(Key::of(Kind::Model, Some(&slug), &model.name), model.id.0);
			index.insert(
				(ResourceKind::Model.as_str(), model.id.0),
				(slug.clone(), model.name.clone()),
			);
			out.models.insert(
				model.name.clone(),
				ModelDoc {
					server: lookup.server(model.server_id),
					repo,
					revision,
					engine: wire(model.engine),
					image: custom.then(|| model.image.clone()),
					port: custom.then(|| u16::try_from(model.port).ok()).flatten(),
					model_path: Some(model.model_path.clone()),
					served_name: Some(model.served_name.clone()),
					context_size: u32::try_from(model.context_size).ok(),
					gpu: Some(model.gpu),
					extra_args: model.extra_args.0.clone(),
					memory_mb: model.memory_limit_mb,
				},
			);
		}
		doc.environments.insert(slug, out);
	}

	// User networks with a member of this project.
	let mut found: std::collections::HashSet<crate::ids::NetworkId> =
		std::collections::HashSet::new();
	let mut user_networks = Vec::new();
	for server in lookup.servers.values() {
		for network in networks::list_by_server(db, project.team_id, *server).await? {
			if network.is_user() && found.insert(network.id) {
				user_networks.push(network);
			}
		}
	}
	let all_ids: Vec<_> = user_networks.iter().map(|n| n.id).collect();
	let members = networks::members(db, &all_ids).await?;
	for network in user_networks {
		let mut listed: Vec<MemberDoc> = members
			.iter()
			.filter(|m| m.network_id == network.id)
			.filter_map(|m| {
				let kind = m.kind()?;
				let (environment, name) = index.get(&(kind.as_str(), m.resource_id))?;
				let mut aliases = m.aliases.clone();
				aliases.sort();
				if aliases == [default_alias(name)] {
					aliases.clear();
				}
				Some(MemberDoc {
					environment: environment.clone(),
					kind: kind.as_str().to_owned(),
					name: name.clone(),
					aliases,
				})
			})
			.collect();
		if listed.is_empty() {
			continue;
		}
		listed.sort_by(|a, b| {
			(&a.environment, &a.kind, &a.name).cmp(&(&b.environment, &b.kind, &b.name))
		});
		let server = lookup.server(network.server_id);
		ids.networks
			.insert(format!("{server}/{}", network.name), network.id);
		doc.networks.push(NetworkDoc {
			name: network.name.clone(),
			server,
			internal: network.internal,
			members: listed,
		});
	}
	doc.networks
		.sort_by(|a, b| (&a.server, &a.name).cmp(&(&b.server, &b.name)));
	Ok(Snapshot { doc, secrets, ids })
}
