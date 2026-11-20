//! The project graph: what the canvas draws. Nodes are a project's resources, groups are its
//! environments and the networks they sit on, edges are the links between resources.
//!
//! Nothing here is stored. The graph is computed from the tables that already hold the truth
//! (resources, network memberships, model links, and the env vars an application was given),
//! so the canvas can never disagree with the pages of the resources themselves.
//!
//! - `rules`: which two nodes may be wired and what wiring them changes (pure).
//! - `layout`: the deterministic layered layout (pure).
//! - `wire`: applies a plan through the existing services, and undoes it.

pub mod layout;
pub mod rules;
pub mod wire;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fmt;

use super::Project;
use super::overview::{self, Pick, ResourceKind, ResourceStatus};
use crate::auth::permission::{Action, Resource, authorize};
use crate::environments::{self, Environment};
use crate::error::AppResult;
use crate::ids::{EnvironmentId, NetworkId, ProjectId, ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::{env_vars, scope};

/// A resource of the graph: its kind and id. The wire format is `a-12`, `d-3`, `m-5`, `s-2`
/// (application, database, model, stack); it is what `id="n-…"`, `data-targets` and the
/// signals carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey {
	pub kind: ResourceKind,
	pub id: i64,
}

impl NodeKey {
	#[must_use]
	pub const fn new(kind: ResourceKind, id: i64) -> Self {
		Self { kind, id }
	}

	const fn prefix(kind: ResourceKind) -> char {
		match kind {
			ResourceKind::Application => 'a',
			ResourceKind::Database => 'd',
			ResourceKind::Model => 'm',
			ResourceKind::Stack => 's',
		}
	}

	/// `a-12` back into a key; anything else is `None`.
	#[must_use]
	pub fn parse(text: &str) -> Option<Self> {
		let (prefix, id) = text.split_once('-')?;
		let kind = [
			ResourceKind::Application,
			ResourceKind::Database,
			ResourceKind::Model,
			ResourceKind::Stack,
		]
		.into_iter()
		.find(|k| prefix.len() == 1 && prefix.starts_with(Self::prefix(*k)))?;
		let id: i64 = id.parse().ok()?;
		(id > 0).then_some(Self { kind, id })
	}
}

impl fmt::Display for NodeKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}-{}", Self::prefix(self.kind), self.id)
	}
}

/// How serious the worst scan finding of an image is. Fed by the image scan (S1); until a
/// resource has a scan, [`Node::scan`] is `None` and the canvas draws no badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "the image scan (S1) constructs the levels")]
pub enum Severity {
	Critical,
	High,
	Medium,
	Low,
}

impl Severity {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Critical => "Critical",
			Self::High => "High",
			Self::Medium => "Medium",
			Self::Low => "Low",
		}
	}

	#[must_use]
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Critical => "critical",
			Self::High => "high",
			Self::Medium => "medium",
			Self::Low => "low",
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scan {
	pub level: Severity,
	pub count: u32,
}

/// A network a resource is recorded on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRef {
	pub id: NetworkId,
	/// What the frame and the lists call it: the Podman name of an environment's network, the
	/// name a person gave a user network.
	pub label: String,
	/// An environment's own network (`auto`), as opposed to one a person made (`user`).
	pub auto: bool,
}

#[derive(Debug, Clone)]
pub struct Node {
	pub key: NodeKey,
	pub name: String,
	/// The one fact under the name: the repository, the engine and version, `4 services`.
	pub detail: String,
	pub mark: Option<&'static str>,
	pub logo_url: Option<String>,
	pub status: ResourceStatus,
	pub environment: EnvironmentId,
	pub server: ServerId,
	/// The networks it is recorded on, an environment's first.
	pub nets: Vec<NetRef>,
	/// A database's connection variable (`DATABASE_URL`, `REDIS_URL`).
	pub var: Option<String>,
	/// The names of the env vars an application has (never the values).
	pub env_keys: Vec<String>,
	pub scan: Option<Scan>,
}

impl Node {
	/// The network the canvas draws the node in: the first one a person made, else its
	/// environment's, else none.
	#[must_use]
	pub fn frame_net(&self) -> Option<&NetRef> {
		self.nets
			.iter()
			.find(|n| !n.auto)
			.or_else(|| self.nets.first())
	}

	#[must_use]
	pub fn is_consumer(&self) -> bool {
		matches!(
			self.key.kind,
			ResourceKind::Application | ResourceKind::Stack
		)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeKind {
	/// An application got a database's connection variable.
	Variable,
	/// An application reaches a model through the podkit gateway.
	Gateway,
	/// Two resources share a network a person made, drawn when they sit in different frames.
	Network,
}

impl EdgeKind {
	#[must_use]
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Variable => "env",
			Self::Gateway => "gateway",
			Self::Network => "network",
		}
	}

	#[must_use]
	pub fn from_slug(slug: &str) -> Option<Self> {
		[Self::Variable, Self::Gateway, Self::Network]
			.into_iter()
			.find(|k| k.slug() == slug)
	}
}

/// A link, always consumer to provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
	pub from: NodeKey,
	pub to: NodeKey,
	pub kind: EdgeKind,
	/// The variable name, `gateway`, or the network's name.
	pub label: String,
	/// Set on a network edge: the network it is about.
	pub network: Option<NetworkId>,
}

impl Edge {
	/// A stable id for the DOM: `e-a-1-d-2-env`.
	#[must_use]
	pub fn dom_id(&self) -> String {
		format!("e-{}-{}-{}", self.from, self.to, self.kind.slug())
	}
}

#[derive(Debug, Clone)]
pub struct Graph {
	pub project: Project,
	pub environments: Vec<Environment>,
	pub nodes: Vec<Node>,
	/// Sorted by (from, to, kind, label), so the output is the same for the same data.
	pub edges: Vec<Edge>,
}

impl Graph {
	#[must_use]
	pub fn node(&self, key: NodeKey) -> Option<&Node> {
		self.nodes.iter().find(|n| n.key == key)
	}

	#[must_use]
	pub fn environment(&self, id: EnvironmentId) -> Option<&Environment> {
		self.environments.iter().find(|e| e.id == id)
	}

	/// The edges with `key` at either end.
	pub fn edges_of(&self, key: NodeKey) -> impl Iterator<Item = &Edge> {
		self.edges
			.iter()
			.filter(move |e| e.from == key || e.to == key)
	}
}

/// The connection variable a database of `engine` offers an application.
#[must_use]
pub fn engine_var(engine: &str) -> &'static str {
	match engine {
		"mysql" => "MYSQL_URL",
		"mongodb" => "MONGODB_URL",
		"redis" => "REDIS_URL",
		_ => "DATABASE_URL",
	}
}

struct Home {
	key: NodeKey,
	server: ServerId,
	engine: Option<String>,
}

/// Where each resource of the project runs, and a database's engine.
async fn homes(s: &AppState, project: ProjectId) -> Result<Vec<Home>, sqlx::Error> {
	let rows: Vec<(String, i64, i64, Option<String>)> = sqlx::query_as(
		"SELECT 'application', id, server_id, NULL::text FROM applications WHERE project_id = $1
		UNION ALL SELECT 'database', id, server_id, engine::text FROM databases WHERE project_id = $1
		UNION ALL SELECT 'model', id, server_id, NULL::text FROM models WHERE project_id = $1
		UNION ALL SELECT 'stack', id, server_id, NULL::text FROM stacks WHERE project_id = $1",
	)
	.bind(project)
	.fetch_all(&s.db)
	.await?;
	Ok(rows
		.into_iter()
		.filter_map(|(kind, id, server, engine)| {
			let kind = ResourceKind::from_slug(&kind)?;
			Some(Home {
				key: NodeKey::new(kind, id),
				server: ServerId(server),
				engine,
			})
		})
		.collect())
}

/// The networks the project's resources are recorded on.
async fn memberships(
	s: &AppState,
	team: TeamId,
	project: ProjectId,
) -> Result<Vec<(NodeKey, NetRef)>, sqlx::Error> {
	let rows: Vec<(String, i64, NetworkId, String, String, String)> = sqlx::query_as(
		"SELECT rn.resource_kind, rn.resource_id, n.id, n.kind, n.name, n.podman_name
		FROM resource_networks rn JOIN networks n ON n.id = rn.network_id
		WHERE n.team_id = $1 AND (
			(rn.resource_kind = 'application' AND rn.resource_id IN
				(SELECT id FROM applications WHERE project_id = $2))
			OR (rn.resource_kind = 'database' AND rn.resource_id IN
				(SELECT id FROM databases WHERE project_id = $2))
			OR (rn.resource_kind = 'model' AND rn.resource_id IN
				(SELECT id FROM models WHERE project_id = $2))
			OR (rn.resource_kind = 'stack' AND rn.resource_id IN
				(SELECT id FROM stacks WHERE project_id = $2)))
		ORDER BY (n.kind <> 'environment'), n.name, n.id",
	)
	.bind(team)
	.bind(project)
	.fetch_all(&s.db)
	.await?;
	Ok(rows
		.into_iter()
		.filter_map(|(kind, id, net, net_kind, name, podman)| {
			let kind = ResourceKind::from_slug(&kind)?;
			let auto = net_kind == crate::networks::KIND_ENVIRONMENT;
			Some((
				NodeKey::new(kind, id),
				NetRef {
					id: net,
					label: if auto { podman } else { name },
					auto,
				},
			))
		})
		.collect())
}

/// Application to model links among the project's applications.
async fn model_links(s: &AppState, project: ProjectId) -> Result<Vec<(i64, i64)>, sqlx::Error> {
	sqlx::query_as(
		"SELECT am.application_id, am.model_id FROM application_models am
		JOIN applications a ON a.id = am.application_id
		WHERE a.project_id = $1 ORDER BY am.application_id, am.model_id",
	)
	.bind(project)
	.fetch_all(&s.db)
	.await
}

fn node_of(card: overview::ResourceCard, environment: EnvironmentId, home: &Home) -> Node {
	Node {
		key: NodeKey::new(card.kind, card.id),
		name: card.name,
		detail: card.detail,
		mark: card.mark,
		logo_url: card.logo_url,
		status: card.status,
		environment,
		server: home.server,
		nets: Vec::new(),
		var: home
			.engine
			.as_deref()
			.map(|e| engine_var(e).to_owned())
			.filter(|_| card.kind == ResourceKind::Database),
		env_keys: Vec::new(),
		scan: None,
	}
}

/// The host a database answers to on its environment's network, as it appears in a
/// connection URL after the credentials.
#[must_use]
pub fn db_host_marker(id: i64) -> String {
	format!("@podkit-db-{id}:")
}

/// Builds the graph of a project; needs `read project`. A project of another team is
/// `ProjectNotFound`. A kind of resource the caller may not read is left out, like in the
/// environment view; so are the variable links of a caller who may not read env vars.
///
/// # Errors
/// `Forbidden`, `ProjectNotFound`, or the database failing.
pub async fn build(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<Graph> {
	authorize(&s.db, user, team, Action::Read, Resource::Project).await?;
	let project = scope::project(&s.db, team, project).await?;
	let environments = environments::list_by_project(&s.db, project.id).await?;
	let homes = homes(s, project.id).await?;

	let mut nodes = Vec::new();
	for env in &environments {
		let view =
			overview::environment_view(&s.db, user, team, project.id, Pick::Id(env.id)).await?;
		for card in view.resources {
			let key = NodeKey::new(card.kind, card.id);
			if let Some(home) = homes.iter().find(|h| h.key == key) {
				nodes.push(node_of(card, env.id, home));
			}
		}
	}

	for (key, net) in memberships(s, team, project.id).await? {
		if let Some(node) = nodes.iter_mut().find(|n| n.key == key) {
			node.nets.push(net);
		}
	}

	let mut edges = Vec::new();

	// An application got a database's variable when one of its env vars holds a URL that
	// points at the database's host. The values are read here and never leave this function.
	if authorize(&s.db, user, team, Action::Read, Resource::EnvVar)
		.await
		.is_ok()
	{
		let dbs: Vec<(NodeKey, EnvironmentId)> = nodes
			.iter()
			.filter(|n| n.key.kind == ResourceKind::Database)
			.map(|n| (n.key, n.environment))
			.collect();
		for node in nodes
			.iter_mut()
			.filter(|n| n.key.kind == ResourceKind::Application)
		{
			let vars = env_vars::list(&s.db, crate::ids::ApplicationId(node.key.id)).await?;
			for var in vars {
				node.env_keys.push(var.key.clone());
				let Ok(value) = s.secrets.decrypt(&var.value) else {
					continue;
				};
				for (db, db_env) in &dbs {
					if *db_env == node.environment && value.contains(&db_host_marker(db.id)) {
						edges.push(Edge {
							from: node.key,
							to: *db,
							kind: EdgeKind::Variable,
							label: var.key.clone(),
							network: None,
						});
					}
				}
			}
			node.env_keys.sort();
		}
	}

	for (app, model) in model_links(s, project.id).await? {
		let from = NodeKey::new(ResourceKind::Application, app);
		let to = NodeKey::new(ResourceKind::Model, model);
		if nodes.iter().any(|n| n.key == from) && nodes.iter().any(|n| n.key == to) {
			edges.push(Edge {
				from,
				to,
				kind: EdgeKind::Gateway,
				label: "gateway".to_owned(),
				network: None,
			});
		}
	}

	edges.extend(network_edges(&nodes));
	edges.sort_by(|a, b| (a.from, a.to, a.kind, &a.label).cmp(&(b.from, b.to, b.kind, &b.label)));
	edges.dedup();

	Ok(Graph {
		project,
		environments,
		nodes,
		edges,
	})
}

/// A person-made network joins resources that the canvas draws in different frames (one of
/// them is drawn in another such network): a dashed edge from the first member to each of
/// those. Resources in one frame need no edge, the frame is the tie.
fn network_edges(nodes: &[Node]) -> Vec<Edge> {
	let mut by_net: HashMap<NetworkId, (&NetRef, Vec<&Node>)> = HashMap::new();
	for node in nodes {
		for net in node.nets.iter().filter(|n| !n.auto) {
			by_net
				.entry(net.id)
				.or_insert((net, Vec::new()))
				.1
				.push(node);
		}
	}
	let mut out = Vec::new();
	for (net, mut members) in by_net.into_values() {
		members.sort_by_key(|n| n.key);
		let Some((first, rest)) = members.split_first() else {
			continue;
		};
		for other in rest {
			if first.frame_net() != other.frame_net() {
				out.push(Edge {
					from: first.key,
					to: other.key,
					kind: EdgeKind::Network,
					label: net.label.clone(),
					network: Some(net.id),
				});
			}
		}
	}
	out
}
