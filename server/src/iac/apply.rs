//! Applying a plan: each change goes through the same service the web UI and the JSON API use,
//! so every permission, validation and job of those applies here too.
//!
//! The changes run in dependency order and stop at the first one that fails; the report says
//! what was done and what was not. Running the same file again plans nothing.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use super::doc::{AppDoc, ProjectDoc};
use super::objects::{
	DEFAULT_BUILD, DEFAULT_DOCKERFILE, DEFAULT_REF, Key, Kind, Resolved, Secrets, default_alias,
	is_collection,
};
use super::plan::{Action, Change, FieldChange, Plan};
use super::snapshot::{Ids, Lookup};
use crate::applications::service::AppInput;
use crate::change::Change as Set;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, DatabaseId, EnvironmentId, ModelId, NetworkId, ProjectId, RegistryId, ServerId,
	StackId, TeamId, UserId,
};
use crate::networks::ResourceKind;
use crate::state::AppState;
use crate::{
	applications, databases, deployments, domains, env_vars, environments, links, models, networks,
	stacks, volumes,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
	/// Deploy applications and stacks that were created or changed.
	pub deploy: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Done {
	pub action: Action,
	pub kind: Kind,
	pub environment: Option<String>,
	pub name: String,
}

impl Done {
	fn of(change: &Change) -> Self {
		Self {
			action: change.action,
			kind: change.kind,
			environment: change.environment.clone(),
			name: change.name.clone(),
		}
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct Failure {
	pub kind: Kind,
	pub environment: Option<String>,
	pub name: String,
	pub message: String,
}

impl Failure {
	fn of(change: &Change, message: String) -> Self {
		Self {
			kind: change.kind,
			environment: change.environment.clone(),
			name: change.name.clone(),
			message,
		}
	}
}

/// What an apply did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
	/// Done, in order.
	pub applied: Vec<Done>,
	/// The change that stopped the apply; the ones after it did not run.
	pub failed: Option<Failure>,
	/// Not run because of the failure.
	pub not_run: Vec<Done>,
	/// Deploys queued for applications and stacks.
	pub deployed: Vec<Done>,
}

impl Report {
	#[must_use]
	pub fn ok(&self) -> bool {
		self.failed.is_none()
	}
}

pub struct Cx<'a> {
	pub state: &'a AppState,
	pub user: UserId,
	pub team: TeamId,
	pub project: ProjectId,
	pub doc: &'a ProjectDoc,
	pub secrets: &'a Secrets,
	pub lookup: &'a Lookup,
	pub ids: Ids,
}

fn invalid(message: impl Into<String>) -> ServerError {
	ServerError::Validation(message.into())
}

fn field<'v>(change: &'v Change, name: &str) -> Option<&'v FieldChange> {
	change.fields.iter().find(|f| f.field == name)
}

impl Cx<'_> {
	fn plain(&self, raw: &str) -> AppResult<String> {
		match self.secrets.resolve(raw) {
			Resolved::Known(v) => Ok(v),
			Resolved::Missing(name) => {
				Err(invalid(format!("the secret \"{name}\" is not provided")))
			}
		}
	}

	fn opt_plain(&self, raw: Option<&String>) -> AppResult<Option<String>> {
		raw.map(|r| self.plain(r)).transpose()
	}

	fn environment(&self, change: &Change) -> AppResult<EnvironmentId> {
		let slug = change.environment.as_deref().unwrap_or_default();
		self.ids
			.environments
			.get(slug)
			.copied()
			.ok_or_else(|| invalid(format!("environment \"{slug}\" does not exist")))
	}

	fn server(&self, name: &str) -> AppResult<ServerId> {
		self.lookup
			.servers
			.get(name)
			.copied()
			.ok_or_else(|| invalid(format!("no server named \"{name}\" in this team")))
	}

	fn key(change: &Change) -> Key {
		Key {
			kind: change.kind,
			environment: change.environment.clone(),
			name: change.name.clone(),
		}
	}

	fn id_of(&self, change: &Change) -> AppResult<i64> {
		self.ids
			.objects
			.get(&Self::key(change))
			.copied()
			.ok_or_else(|| {
				invalid(format!(
					"{} \"{}\" does not exist",
					change.kind.as_str(),
					change.name
				))
			})
	}

	fn env_doc(&self, change: &Change) -> AppResult<&super::doc::EnvironmentDoc> {
		self.doc
			.environments
			.get(change.environment.as_deref().unwrap_or_default())
			.ok_or_else(|| invalid("the environment is not in the file"))
	}

	/// Runs one change. A network's members wait for the resources they name.
	pub async fn run(&mut self, change: &Change) -> AppResult<()> {
		match (change.kind, change.action) {
			(Kind::Environment, Action::Create | Action::Update) => {
				self.environment_set(change).await
			}
			(Kind::Environment, Action::Delete) => self.environment_delete(change).await,
			(Kind::Network, Action::Create | Action::Update) => self.network_set(change).await,
			(Kind::Network, Action::Delete) => self.network_delete(change).await,
			(Kind::Database, Action::Create) => self.database_create(change).await,
			(Kind::Database, Action::Update) => self.database_update(change).await,
			(Kind::Database, Action::Delete) => self.database_delete(change).await,
			(Kind::Model, Action::Create) => self.model_create(change).await,
			(Kind::Model, Action::Update) => Ok(()),
			(Kind::Model, Action::Delete) => self.model_delete(change).await,
			(Kind::Stack, Action::Create) => self.stack_create(change).await,
			(Kind::Stack, Action::Update) => self.stack_update(change).await,
			(Kind::Stack, Action::Delete) => self.stack_delete(change).await,
			(Kind::App, Action::Create | Action::Update) => self.app_set(change).await,
			(Kind::App, Action::Delete) => self.app_delete(change).await,
		}
	}

	// ----- environments

	async fn environment_set(&mut self, change: &Change) -> AppResult<()> {
		let slug = change.name.clone();
		let env = self.env_doc(change)?;
		let s = self.state;
		if change.action == Action::Create {
			let made = environments::service::create(
				&s.db,
				self.user,
				self.team,
				self.project,
				&env.name,
				&slug,
			)
			.await?;
			self.ids.environments.insert(slug.clone(), made.id);
			self.ids
				.objects
				.insert(Key::of(Kind::Environment, Some(&slug), &slug), made.id.0);
		} else {
			let id = self.environment(change)?;
			environments::service::rename(
				&s.db,
				self.user,
				self.team,
				self.project,
				id,
				&env.name,
				None,
			)
			.await?;
		}
		Ok(())
	}

	async fn environment_delete(&mut self, change: &Change) -> AppResult<()> {
		let id = self.environment(change)?;
		environments::service::remove(&self.state.db, self.user, self.team, self.project, id)
			.await?;
		self.ids.environments.remove(&change.name);
		Ok(())
	}

	// ----- networks

	async fn network_set(&mut self, change: &Change) -> AppResult<()> {
		if change.action != Action::Create {
			return Ok(());
		}
		let doc = self
			.doc
			.networks
			.iter()
			.find(|n| format!("{}/{}", n.server, n.name) == change.name)
			.ok_or_else(|| invalid("the network is not in the file"))?;
		let server = self.server(&doc.server)?;
		let made = networks::service::create(
			self.state,
			self.user,
			self.team,
			server,
			&doc.name,
			doc.internal,
		)
		.await?;
		self.ids.networks.insert(change.name.clone(), made.id);
		Ok(())
	}

	/// Puts the members in place, once what they name exists.
	async fn network_members(&mut self, change: &Change) -> AppResult<()> {
		let Some(network) = self.ids.networks.get(&change.name).copied() else {
			return Ok(());
		};
		for f in change
			.fields
			.iter()
			.filter(|f| f.field.starts_with("member."))
		{
			let spec = &f.field["member.".len()..];
			let mut parts = spec.splitn(3, '/');
			let (Some(env), Some(kind), Some(name)) = (parts.next(), parts.next(), parts.next())
			else {
				continue;
			};
			let kind = ResourceKind::parse(kind)
				.ok_or_else(|| invalid(format!("\"{kind}\" is not a kind of resource")))?;
			let key_kind = match kind {
				ResourceKind::Application => Kind::App,
				ResourceKind::Database => Kind::Database,
				ResourceKind::Model => Kind::Model,
				ResourceKind::Stack => Kind::Stack,
			};
			let resource = self
				.ids
				.objects
				.get(&Key::of(key_kind, Some(env), name))
				.copied()
				.ok_or_else(|| invalid(format!("{} \"{name}\" does not exist", kind.as_str())))?;
			if let Some(to) = &f.to {
				let aliases: Vec<String> = serde_json::from_value(to.clone()).unwrap_or_default();
				let aliases = if aliases == [default_alias(name)] {
					Vec::new()
				} else {
					aliases
				};
				networks::service::attach(
					self.state, self.user, self.team, network, kind, resource, &aliases,
				)
				.await?;
			} else {
				networks::service::detach(
					self.state, self.user, self.team, network, kind, resource,
				)
				.await?;
			}
		}
		Ok(())
	}

	async fn network_delete(&mut self, change: &Change) -> AppResult<()> {
		let id: NetworkId = self
			.ids
			.networks
			.get(&change.name)
			.copied()
			.ok_or_else(|| invalid("the network does not exist"))?;
		networks::service::delete(self.state, self.user, self.team, id).await
	}

	// ----- databases

	async fn database_create(&mut self, change: &Change) -> AppResult<()> {
		let env = self.environment(change)?;
		let doc = self
			.env_doc(change)?
			.databases
			.get(&change.name)
			.ok_or_else(|| invalid("the database is not in the file"))?;
		let engine: databases::EngineKind =
			serde_json::from_value(Value::String(doc.engine.clone()))
				.map_err(|_| invalid(format!("\"{}\" is not a database engine", doc.engine)))?;
		let made = databases::service::create(
			self.state,
			self.user,
			self.team,
			self.project,
			databases::service::CreateInput {
				name: change.name.clone(),
				engine,
				version: doc.version.clone(),
				server_id: self.server(&doc.server)?,
				public_port: doc.public_port,
				memory_limit_mb: doc.memory_mb,
				cpu_limit: doc.cpu,
				environment_id: Some(env),
			},
		)
		.await?;
		self.ids.objects.insert(Self::key(change), made.id.0);
		Ok(())
	}

	async fn database_update(&mut self, change: &Change) -> AppResult<()> {
		let id = DatabaseId(self.id_of(change)?);
		let doc = self
			.env_doc(change)?
			.databases
			.get(&change.name)
			.ok_or_else(|| invalid("the database is not in the file"))?;
		let current = crate::scope::database(&self.state.db, self.team, self.project, id).await?;
		databases::service::update_settings(
			self.state,
			self.user,
			self.team,
			self.project,
			id,
			databases::service::SettingsInput {
				public_port: doc.public_port,
				memory_limit_mb: doc.memory_mb,
				cpu_limit: doc.cpu,
				backup_target_id: current.backup_target_id,
				backup_schedule: current.backup_schedule.clone(),
				backup_keep: Some(current.backup_keep),
			},
		)
		.await?;
		Ok(())
	}

	async fn database_delete(&mut self, change: &Change) -> AppResult<()> {
		let id = DatabaseId(self.id_of(change)?);
		databases::service::remove(self.state, self.user, self.team, self.project, id, false)
			.await?;
		Ok(())
	}

	// ----- models

	async fn model_create(&mut self, change: &Change) -> AppResult<()> {
		let env = self.environment(change)?;
		let doc = self
			.env_doc(change)?
			.models
			.get(&change.name)
			.ok_or_else(|| invalid("the model is not in the file"))?;
		let engine: models::EngineKind = serde_json::from_value(Value::String(doc.engine.clone()))
			.map_err(|_| invalid(format!("\"{}\" is not a model engine", doc.engine)))?;
		let file = self
			.lookup
			.model_files
			.get(&(doc.server.clone(), doc.repo.clone(), doc.revision.clone()))
			.copied()
			.ok_or_else(|| {
				invalid(format!(
					"no model download of {} at {} on server \"{}\"",
					doc.repo, doc.revision, doc.server
				))
			})?;
		let made = models::service::create(
			self.state,
			self.user,
			self.team,
			self.project,
			models::service::CreateModel {
				name: change.name.clone(),
				model_files_id: file,
				engine,
				served_name: doc.served_name.clone(),
				model_path: doc.model_path.clone(),
				context_size: doc.context_size,
				gpu: doc.gpu,
				extra_args: doc.extra_args.clone(),
				image: doc.image.clone(),
				port: doc.port,
				memory_limit_mb: doc.memory_mb,
				environment_id: Some(env),
			},
		)
		.await?;
		self.ids.models.insert(change.name.clone(), made.id);
		self.ids.objects.insert(Self::key(change), made.id.0);
		Ok(())
	}

	async fn model_delete(&mut self, change: &Change) -> AppResult<()> {
		let id = ModelId(self.id_of(change)?);
		models::service::remove(self.state, self.user, self.team, self.project, id).await?;
		self.ids.models.remove(&change.name);
		Ok(())
	}

	// ----- stacks

	fn registry(&self, name: Option<&String>) -> AppResult<Option<RegistryId>> {
		name.map(|n| {
			self.lookup
				.registries
				.get(n)
				.copied()
				.ok_or_else(|| invalid(format!("no registry named \"{n}\" in this team")))
		})
		.transpose()
	}

	async fn stack_create(&mut self, change: &Change) -> AppResult<()> {
		let env = self.environment(change)?;
		let doc = self
			.env_doc(change)?
			.stacks
			.get(&change.name)
			.ok_or_else(|| invalid("the stack is not in the file"))?;
		let mut vars = BTreeMap::new();
		for (key, raw) in &doc.env {
			vars.insert(key.clone(), self.plain(raw)?);
		}
		let mut files = BTreeMap::new();
		for (path, raw) in &doc.files {
			files.insert(path.clone(), self.plain(raw)?);
		}
		let (made, _) = stacks::service::create(
			self.state,
			self.user,
			self.team,
			self.project,
			stacks::service::CreateInput {
				name: change.name.clone(),
				compose_yaml: doc.compose.clone(),
				env: vars,
				files,
				server_id: self.server(&doc.server)?,
				environment_id: Some(env),
				source: doc.source.clone(),
				deploy: false,
				registry: self.registry(doc.registry.as_ref())?,
			},
		)
		.await?;
		self.ids.objects.insert(Self::key(change), made.id.0);
		self.stack_domains(change, made.id).await
	}

	async fn stack_domains(&self, change: &Change, id: StackId) -> AppResult<()> {
		for f in change
			.fields
			.iter()
			.filter(|f| f.field.starts_with("domain."))
		{
			let service = &f.field["domain.".len()..];
			if let Some(Value::String(target)) = &f.to {
				let (host, port) = match target.rsplit_once(':') {
					Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse::<u16>().ok()),
					_ => (target.as_str(), None),
				};
				stacks::service::set_domain(
					self.state,
					self.user,
					self.team,
					self.project,
					id,
					service,
					host,
					port,
				)
				.await?;
			} else {
				stacks::service::clear_domain(
					self.state,
					self.user,
					self.team,
					self.project,
					id,
					service,
				)
				.await?;
			}
		}
		Ok(())
	}

	async fn stack_update(&mut self, change: &Change) -> AppResult<()> {
		let id = StackId(self.id_of(change)?);
		let doc = self
			.env_doc(change)?
			.stacks
			.get(&change.name)
			.ok_or_else(|| invalid("the stack is not in the file"))?;
		let mut input = stacks::service::UpdateInput::default();
		if field(change, "compose").is_some() {
			input.compose_yaml = Some(doc.compose.clone());
		}
		for f in change.fields.iter().filter(|f| f.field.starts_with("env.")) {
			let key = f.field["env.".len()..].to_owned();
			if f.to.is_some() {
				let raw = doc
					.env
					.get(&key)
					.ok_or_else(|| invalid("variable is not in the file"))?;
				input.env_set.insert(key, self.plain(raw)?);
			} else {
				input.env_unset.push(key);
			}
		}
		if field(change, "registry").is_some() {
			input.registry = match self.registry(doc.registry.as_ref())? {
				Some(id) => Set::Set(id),
				None => Set::Clear,
			};
		}
		let touched = input.compose_yaml.is_some()
			|| !input.env_set.is_empty()
			|| !input.env_unset.is_empty()
			|| input.registry.is_touched();
		if touched {
			stacks::service::update(self.state, self.user, self.team, self.project, id, input)
				.await?;
		}
		self.stack_domains(change, id).await
	}

	async fn stack_delete(&mut self, change: &Change) -> AppResult<()> {
		let id = StackId(self.id_of(change)?);
		stacks::service::remove(self.state, self.user, self.team, self.project, id, false).await?;
		Ok(())
	}

	// ----- applications

	fn app_input(
		&self,
		name: &str,
		app: &AppDoc,
		env: Option<EnvironmentId>,
		update: bool,
	) -> AppResult<AppInput> {
		let git = app.image.is_none();
		let registry = match self.registry(app.registry.as_ref())? {
			Some(id) => Set::Set(id),
			None if update => Set::Clear,
			None => Set::Keep,
		};
		let deploy_key_id =
			match &app.deploy_key {
				Some(key) => {
					Some(self.lookup.ssh_keys.get(key).copied().ok_or_else(|| {
						invalid(format!("no SSH key named \"{key}\" in this team"))
					})?)
				}
				None => None,
			};
		Ok(AppInput {
			name: name.to_owned(),
			slug: app.slug.clone(),
			repo_url: app.repo.clone().unwrap_or_default(),
			image: app.image.clone(),
			registry_username: self.opt_plain(app.registry_username.as_ref())?,
			registry_password: self.opt_plain(app.registry_password.as_ref())?,
			registry,
			git_ref: if git {
				app.git_ref
					.clone()
					.unwrap_or_else(|| DEFAULT_REF.to_owned())
			} else {
				DEFAULT_REF.to_owned()
			},
			deploy_key: None,
			deploy_key_id,
			git_provider_id: None,
			git_repo: String::new(),
			generate_deploy_key: false,
			git_token: self.opt_plain(app.git_token.as_ref())?,
			build_strategy: app
				.build
				.clone()
				.unwrap_or_else(|| DEFAULT_BUILD.to_owned()),
			dockerfile_path: app
				.dockerfile
				.clone()
				.unwrap_or_else(|| DEFAULT_DOCKERFILE.to_owned()),
			container_port: app.port,
			memory_limit_mb: app.memory_mb,
			cpu_limit: app.cpu,
			environment_id: env,
		})
	}

	#[allow(clippy::too_many_lines, reason = "one arm per field of an application")]
	async fn app_set(&mut self, change: &Change) -> AppResult<()> {
		let s = self.state;
		let id = if change.action == Action::Create {
			let env = self.environment(change)?;
			let doc = self
				.env_doc(change)?
				.apps
				.get(&change.name)
				.ok_or_else(|| invalid("the application is not in the file"))?;
			let input = self.app_input(&change.name, doc, Some(env), false)?;
			let server = self.server(&doc.server)?;
			let created =
				applications::service::create(s, self.user, self.team, self.project, server, input)
					.await?;
			self.ids.objects.insert(Self::key(change), created.app.id.0);
			created.app.id
		} else {
			let id = ApplicationId(self.id_of(change)?);
			if change.fields.iter().any(|f| !is_collection(&f.field)) {
				let doc = self
					.env_doc(change)?
					.apps
					.get(&change.name)
					.ok_or_else(|| invalid("the application is not in the file"))?;
				let input = self.app_input(&change.name, doc, None, true)?;
				applications::service::update(s, self.user, self.team, self.project, id, input)
					.await?;
			}
			id
		};
		let doc = self
			.env_doc(change)?
			.apps
			.get(&change.name)
			.ok_or_else(|| invalid("the application is not in the file"))?;

		for f in &change.fields {
			if let Some(key) = f.field.strip_prefix("env.") {
				if f.to.is_some() {
					let raw = doc
						.env
						.get(key)
						.ok_or_else(|| invalid("variable is not in the file"))?;
					env_vars::service::upsert(
						s,
						self.user,
						self.team,
						self.project,
						id,
						key,
						&self.plain(raw)?,
					)
					.await?;
				} else {
					let keys =
						env_vars::service::list(&s.db, self.user, self.team, self.project, id)
							.await?;
					if let Some(var) = keys.iter().find(|k| k.key == key) {
						env_vars::service::remove(
							&s.db,
							self.user,
							self.team,
							self.project,
							id,
							var.id,
						)
						.await?;
					}
				}
			} else if let Some(host) = f.field.strip_prefix("domain.") {
				if f.to.is_some() {
					domains::service::create(&s.db, self.user, self.team, self.project, id, host)
						.await?;
				} else {
					let list =
						domains::service::list(&s.db, self.user, self.team, self.project, id)
							.await?;
					if let Some(d) = list.domains.iter().find(|d| d.hostname == host) {
						domains::service::remove(
							&s.db,
							self.user,
							self.team,
							self.project,
							id,
							d.id,
						)
						.await?;
					}
				}
			} else if let Some(path) = f.field.strip_prefix("volume.") {
				if f.to.is_some() {
					volumes::service::create(&s.db, self.user, self.team, self.project, id, path)
						.await?;
				} else {
					let list =
						volumes::service::list(&s.db, self.user, self.team, self.project, id)
							.await?;
					if let Some(v) = list.iter().find(|v| v.mount_path == path) {
						volumes::service::remove(s, self.user, self.team, self.project, id, v.id)
							.await?;
					}
				}
			} else if let Some(model) = f.field.strip_prefix("link.") {
				let model_id = self
					.ids
					.models
					.get(model)
					.copied()
					.ok_or_else(|| invalid(format!("no model named \"{model}\"")))?;
				let default = f.to.as_ref().and_then(Value::as_bool).unwrap_or(false);
				if f.to.is_none() {
					links::service::unlink(s, self.user, self.team, self.project, id, model_id)
						.await?;
				} else if f.from.is_none() {
					links::service::link(
						s,
						self.user,
						self.team,
						self.project,
						id,
						model_id,
						default,
					)
					.await?;
				} else if default {
					links::service::set_default(
						s,
						self.user,
						self.team,
						self.project,
						id,
						model_id,
					)
					.await?;
				}
			}
		}
		Ok(())
	}

	async fn app_delete(&mut self, change: &Change) -> AppResult<()> {
		let id = ApplicationId(self.id_of(change)?);
		applications::service::remove(self.state, self.user, self.team, self.project, id).await?;
		Ok(())
	}

	/// Queues a deploy of an application or stack the apply made or changed.
	async fn deploy(&self, change: &Change) -> AppResult<bool> {
		let id = self.id_of(change)?;
		match change.kind {
			Kind::App => {
				deployments::service::deploy(
					self.state,
					self.user,
					self.team,
					self.project,
					ApplicationId(id),
				)
				.await?;
				Ok(true)
			}
			Kind::Stack => {
				stacks::service::deploy(
					self.state,
					self.user,
					self.team,
					self.project,
					StackId(id),
				)
				.await?;
				Ok(true)
			}
			_ => Ok(false),
		}
	}
}

/// Runs `plan` and says what happened. A change that fails stops the run: the creates and
/// updates go first in dependency order, then a network's members, then the deletes.
pub async fn run(cx: &mut Cx<'_>, plan: &Plan, opts: Options) -> Report {
	let mut report = Report::default();
	let (deletes, rest): (Vec<&Change>, Vec<&Change>) = plan
		.changes
		.iter()
		.partition(|c| c.action == Action::Delete);
	let mut ran: Vec<&Change> = Vec::new();

	'work: {
		for change in &rest {
			match cx.run(change).await {
				Ok(()) => {
					report.applied.push(Done::of(change));
					ran.push(change);
				}
				Err(e) => {
					report.failed = Some(Failure::of(change, e.to_string()));
					break 'work;
				}
			}
		}
		for change in rest.iter().filter(|c| c.kind == Kind::Network) {
			if let Err(e) = cx.network_members(change).await {
				report.failed = Some(Failure::of(change, format!("members: {e}")));
				break 'work;
			}
		}
		for change in &deletes {
			match cx.run(change).await {
				Ok(()) => {
					report.applied.push(Done::of(change));
					ran.push(change);
				}
				Err(e) => {
					report.failed = Some(Failure::of(change, e.to_string()));
					break 'work;
				}
			}
		}
		if opts.deploy {
			for change in rest.iter().filter(|c| c.will_redeploy) {
				match cx.deploy(change).await {
					Ok(true) => report.deployed.push(Done::of(change)),
					Ok(false) => {}
					Err(e) => {
						report.failed = Some(Failure::of(change, format!("deploy: {e}")));
						break 'work;
					}
				}
			}
		}
	}

	if report.failed.is_some() {
		report.not_run = rest
			.iter()
			.chain(deletes.iter())
			.filter(|c| !ran.iter().any(|r| std::ptr::eq(*r, **c)))
			.filter(|c| {
				report.failed.as_ref().is_none_or(|f| {
					!(f.kind == c.kind && f.environment == c.environment && f.name == c.name)
				})
			})
			.map(|c| Done::of(c))
			.collect();
	}
	report
}
