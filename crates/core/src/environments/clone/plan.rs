//! What a clone copies, decided without touching the database: [`Plan::build`] turns a
//! [`Snapshot`] of the source environment into the list the preview shows and the copy
//! follows. [`load`] reads the snapshot through the slices that own the rows.

use std::collections::HashMap;

use serde::Serialize;

use super::{Kind, Options, Secrets};
use crate::applications::{self, Application, public_hostname};
use crate::cron::{self, CronTask};
use crate::databases::{self, Database};
use crate::env_vars::{self, EnvVar};
use crate::environments::Environment;
use crate::error::AppResult;
use crate::ids::{NetworkId, ServerId, TeamId};
use crate::links::{self, Link};
use crate::models::{self, Model};
use crate::networks::{self, Network, ResourceKind};
use crate::servers::Server;
use crate::shared_env::{self, Scope, SharedVar};
use crate::stacks::{self, Stack};
use crate::state::AppState;
use crate::volume_backups::subject::stack_volume_keys;
use crate::volumes::{self, Volume};
use crate::{domains, scope};

/// Longest name or slug a resource may have.
const MAX_LEN: usize = 60;

pub struct AppSnap {
	pub app: Application,
	pub env_vars: Vec<EnvVar>,
	pub volumes: Vec<Volume>,
	pub custom_domains: usize,
	pub links: Vec<Link>,
	/// The user networks it is attached to.
	pub networks: Vec<NetworkId>,
}

pub struct DbSnap {
	pub db: Database,
	pub networks: Vec<NetworkId>,
}

pub struct StackSnap {
	pub stack: Stack,
	/// The compose names of its volumes.
	pub volumes: Vec<String>,
	/// How many variables it has.
	pub env_keys: usize,
	pub networks: Vec<NetworkId>,
}

pub struct ModelSnap {
	pub model: Model,
	pub networks: Vec<NetworkId>,
}

/// Everything about the source environment a clone needs.
pub struct Snapshot {
	pub environment: Environment,
	pub servers: HashMap<ServerId, Server>,
	pub apps: Vec<AppSnap>,
	pub databases: Vec<DbSnap>,
	pub stacks: Vec<StackSnap>,
	pub models: Vec<ModelSnap>,
	/// The scheduled tasks of the environment.
	pub cron: Vec<CronTask>,
	/// The variables set at the environment's own scope.
	pub shared: Vec<SharedVar>,
	/// The environment's own networks, one per server.
	pub env_networks: Vec<Network>,
}

impl Snapshot {
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.apps.is_empty()
			&& self.databases.is_empty()
			&& self.stacks.is_empty()
			&& self.models.is_empty()
			&& self.cron.is_empty()
	}

	/// The server the clone job is queued on: the first resource's.
	#[must_use]
	pub fn first_server(&self) -> Option<ServerId> {
		self.apps
			.first()
			.map(|a| a.app.server_id)
			.or_else(|| self.databases.first().map(|d| d.db.server_id))
			.or_else(|| self.stacks.first().map(|s| s.stack.server_id))
			.or_else(|| self.models.first().map(|m| m.model.server_id))
			.or_else(|| self.cron.first().map(|t| t.server_id))
	}

	/// Whether there is anything a data copy would copy: a dumpable database or a volume.
	#[must_use]
	pub fn has_data(&self) -> bool {
		self.databases
			.iter()
			.any(|d| d.db.engine().dump_ext().is_some())
			|| self.apps.iter().any(|a| !a.volumes.is_empty())
			|| self.stacks.iter().any(|s| !s.volumes.is_empty())
	}
}

/// The user networks `kind`/`id` is attached to. Environment networks are made again for
/// the clone on first use, and system networks are managed, so only user ones are copied.
async fn user_networks(
	db: &sqlx::PgPool,
	kind: ResourceKind,
	id: i64,
) -> Result<Vec<NetworkId>, sqlx::Error> {
	Ok(networks::memberships(db, kind, id)
		.await?
		.into_iter()
		.filter(|(n, _)| n.is_user())
		.map(|(n, _)| n.id)
		.collect())
}

/// Reads the environment's resources. The environment must already be scoped to the team.
///
/// # Errors
/// The database fails.
pub async fn load(s: &AppState, team: TeamId, environment: Environment) -> AppResult<Snapshot> {
	let db = &s.db;
	let mut servers: HashMap<ServerId, Server> = HashMap::new();

	let mut apps = Vec::new();
	for app in applications::list_by_environment(db, environment.id).await? {
		if let std::collections::hash_map::Entry::Vacant(e) = servers.entry(app.server_id) {
			e.insert(scope::server(db, team, app.server_id).await?);
		}
		apps.push(AppSnap {
			env_vars: env_vars::list(db, app.id).await?,
			volumes: volumes::list(db, app.id).await?,
			custom_domains: domains::list(db, app.id).await?.len(),
			links: links::list(db, app.id).await?,
			networks: user_networks(db, ResourceKind::Application, app.id.0).await?,
			app,
		});
	}
	let mut dbs = Vec::new();
	for database in databases::list_by_environment(db, environment.id).await? {
		dbs.push(DbSnap {
			networks: user_networks(db, ResourceKind::Database, database.id.0).await?,
			db: database,
		});
	}
	let mut stack_snaps = Vec::new();
	for stack in stacks::list_by_environment(db, environment.id).await? {
		stack_snaps.push(StackSnap {
			volumes: stack_volume_keys(s, &stack),
			env_keys: stacks::decrypt_env(&s.secrets, &stack).map_or(0, |e| e.len()),
			networks: user_networks(db, ResourceKind::Stack, stack.id.0).await?,
			stack,
		});
	}
	let mut model_snaps = Vec::new();
	for model in models::list_by_environment(db, environment.id).await? {
		model_snaps.push(ModelSnap {
			networks: user_networks(db, ResourceKind::Model, model.id.0).await?,
			model,
		});
	}
	let cron =
		cron::store::list_tasks(db, environment.project_id, Some(environment.id), None).await?;
	let shared = shared_env::list_scope(
		db,
		Scope::Environment(team, environment.project_id, environment.id),
	)
	.await?;
	let env_networks = networks::list_by_environment(db, team, environment.id).await?;
	Ok(Snapshot {
		environment,
		servers,
		apps,
		databases: dbs,
		stacks: stack_snaps,
		models: model_snaps,
		cron,
		shared,
		env_networks,
	})
}

/// `base` with `-{env_slug}` after it, cut so the whole stays within `max` characters.
/// The base gives way, never the suffix, and a cut never leaves a dangling hyphen.
#[must_use]
pub fn suffixed(base: &str, env_slug: &str, max: usize) -> String {
	let suffix = format!("-{env_slug}");
	let keep = max.saturating_sub(suffix.chars().count());
	let head: String = base.chars().take(keep).collect();
	let head = head.trim_end_matches(['-', ' ']);
	if head.is_empty() {
		return env_slug.chars().take(max).collect();
	}
	format!("{head}{suffix}")
}

/// One resource the clone will make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
	pub kind: Kind,
	pub name: String,
	pub new_name: String,
	/// Applications, stacks and models have one.
	pub new_slug: Option<String>,
	/// The default domain of an application.
	pub url: Option<String>,
	/// What data is copied with it, when the clone copies data.
	pub data: Vec<String>,
	pub notes: Vec<String>,
}

/// What a clone would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
	pub environment_name: String,
	pub environment_slug: String,
	pub items: Vec<Item>,
	/// Shared variables of the environment that are copied.
	pub shared_vars: usize,
	/// Variables whose value is emptied (secrets blank).
	pub blanked: usize,
	/// Links between applications and models.
	pub links: usize,
	/// Attachments to user networks.
	pub networks: usize,
	/// Custom domains that are not copied.
	pub skipped_domains: usize,
	/// Things the user should know that belong to no single resource.
	pub notes: Vec<String>,
	/// Names and slugs that are taken already; a clone with any is refused.
	pub conflicts: Vec<String>,
}

impl Plan {
	/// The plan for cloning `snap` into a new environment `name` / `slug`.
	#[allow(clippy::too_many_lines, reason = "one block per kind of resource")]
	#[must_use]
	pub fn build(snap: &Snapshot, name: &str, slug: &str, options: &Options) -> Self {
		let blank = options.secrets == Secrets::Blank;
		let mut items = Vec::new();
		let mut blanked = 0;
		let mut links = 0;
		let mut networks = 0;
		let mut skipped_domains = 0;

		for a in &snap.apps {
			let new_slug = suffixed(&a.app.slug, slug, MAX_LEN);
			let mut notes = Vec::new();
			if a.custom_domains > 0 {
				skipped_domains += a.custom_domains;
				notes.push(format!(
					"{} custom domain{} not copied",
					a.custom_domains,
					if a.custom_domains == 1 { "" } else { "s" }
				));
			}
			if blank {
				blanked += a.env_vars.len();
				if !a.env_vars.is_empty() {
					notes.push(format!("{} variable values emptied", a.env_vars.len()));
				}
				if a.app.git_token.is_some() || a.app.registry_auth.is_some() {
					notes.push("stored git and registry logins dropped".to_owned());
				}
			}
			links += a.links.len();
			networks += a.networks.len();
			let data = if options.data {
				a.volumes
					.iter()
					.map(|v| format!("volume {}", v.mount_path))
					.collect()
			} else {
				Vec::new()
			};
			if !options.data && !a.volumes.is_empty() {
				notes.push("volumes start empty".to_owned());
			}
			items.push(Item {
				kind: Kind::Application,
				name: a.app.name.clone(),
				new_name: suffixed(&a.app.name, slug, MAX_LEN),
				url: snap
					.servers
					.get(&a.app.server_id)
					.map(|server| public_hostname(&new_slug, server)),
				new_slug: Some(new_slug),
				data,
				notes,
			});
		}
		for d in &snap.databases {
			let mut notes = vec!["new credentials, no public port".to_owned()];
			let mut data = Vec::new();
			if options.data {
				if d.db.engine().dump_ext().is_some() {
					data.push("dump and restore".to_owned());
				} else {
					notes.push("this engine has no dump, so it starts empty".to_owned());
				}
			}
			networks += d.networks.len();
			items.push(Item {
				kind: Kind::Database,
				name: d.db.name.clone(),
				new_name: suffixed(&d.db.name, slug, MAX_LEN),
				new_slug: None,
				url: None,
				data,
				notes,
			});
		}
		for st in &snap.stacks {
			let mut notes = vec!["domains are set again on the clone".to_owned()];
			if blank {
				notes.push("variable values emptied".to_owned());
				blanked += st.env_keys;
			}
			if !options.data && !st.volumes.is_empty() {
				notes.push("volumes start empty".to_owned());
			}
			networks += st.networks.len();
			items.push(Item {
				kind: Kind::Stack,
				name: st.stack.name.clone(),
				new_name: suffixed(&st.stack.name, slug, MAX_LEN),
				new_slug: Some(suffixed(&st.stack.slug, slug, MAX_LEN)),
				url: None,
				data: if options.data {
					st.volumes.iter().map(|v| format!("volume {v}")).collect()
				} else {
					Vec::new()
				},
				notes,
			});
		}
		for m in &snap.models {
			networks += m.networks.len();
			items.push(Item {
				kind: Kind::Model,
				name: m.model.name.clone(),
				new_name: suffixed(&m.model.name, slug, MAX_LEN),
				new_slug: Some(suffixed(&m.model.slug, slug, MAX_LEN)),
				url: None,
				data: Vec::new(),
				notes: vec!["uses the same downloaded files".to_owned()],
			});
		}

		for t in &snap.cron {
			let note = match (options.deploy, t.enabled) {
				(true, true) => "switched on once the clone is deployed",
				(_, true) => "copied paused: it runs when you resume it",
				_ => "copied paused, as it is in the source",
			};
			items.push(Item {
				kind: Kind::Cron,
				name: t.name.clone(),
				new_name: t.name.clone(),
				new_slug: None,
				url: None,
				data: Vec::new(),
				notes: vec![note.to_owned()],
			});
		}

		let shared_vars = snap.shared.len();
		if blank {
			blanked += snap.shared.iter().filter(|v| v.is_secret).count();
		}

		let mut notes = Vec::new();
		if snap.is_empty() {
			notes.push("The environment is empty, so only the environment is made.".to_owned());
		}
		if !snap.is_empty() {
			notes.push(
				"Variables are copied as written: one that names a resource of the source, such as a database host, still points at it."
					.to_owned(),
			);
			notes.push(
				"Backup schedules, webhooks and public database ports are not copied.".to_owned(),
			);
		}
		if options.data && !snap.has_data() {
			notes.push("There is no data to copy.".to_owned());
		}
		Self {
			environment_name: name.to_owned(),
			environment_slug: slug.to_owned(),
			items,
			shared_vars,
			blanked,
			links,
			networks,
			skipped_domains,
			notes,
			conflicts: Vec::new(),
		}
	}
}
