//! What may be wired to what, and what wiring changes. Pure: the same rules answer the
//! popover's preview, the POST that applies it, and the `data-targets` the canvas lights up.
//! The client's copy of the rules is a convenience; this is the check.

use super::{EdgeKind, Graph, Node, NodeKey};
use crate::auth::permission::{Action, Resource};
use crate::projects::overview::{ResourceKind, ResourceStatus};

/// Why two nodes cannot be wired. The text is what the person reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

/// What a wire means, with the consumer first (a reverse drop is flipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
	/// An application gets the database's connection variable.
	Variable {
		consumer: NodeKey,
		provider: NodeKey,
	},
	/// An application reaches the model through the gateway.
	Gateway { app: NodeKey, model: NodeKey },
	/// Stacks that are not on the environment's network yet join it.
	Network {
		a: NodeKey,
		b: NodeKey,
		join: Vec<NodeKey>,
		network: String,
	},
}

/// One change, in the order it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
	JoinNetwork {
		node: NodeKey,
		network: String,
	},
	SetVariable {
		node: NodeKey,
		key: String,
	},
	LinkModel {
		app: NodeKey,
		model: NodeKey,
	},
	/// Rolling redeploy (a first deploy for a resource that never ran is skipped, the
	/// change applies when it does).
	Redeploy {
		node: NodeKey,
	},
	RemoveVariable {
		node: NodeKey,
		key: String,
	},
	UnlinkModel {
		app: NodeKey,
		model: NodeKey,
	},
	LeaveNetwork {
		node: NodeKey,
		network: String,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
	pub intent: Intent,
	pub steps: Vec<Step>,
}

impl Plan {
	/// The consumer and the provider.
	#[must_use]
	pub fn ends(&self) -> (NodeKey, NodeKey) {
		match &self.intent {
			Intent::Variable { consumer, provider } => (*consumer, *provider),
			Intent::Gateway { app, model } => (*app, *model),
			Intent::Network { a, b, .. } => (*a, *b),
		}
	}

	/// What the caller must be allowed to do for the plan to run to the end, checked before
	/// the first step so a refusal cannot leave half of it done.
	#[must_use]
	pub fn needs(&self) -> Vec<(Action, Resource)> {
		let mut out = vec![(Action::Read, Resource::Project)];
		for step in &self.steps {
			let more = match step {
				Step::JoinNetwork { node, .. } | Step::LeaveNetwork { node, .. } => {
					vec![(Action::Update, permission(node.kind))]
				}
				Step::SetVariable { .. } => vec![
					(Action::Create, Resource::EnvVar),
					(Action::Read, Resource::Database),
					(Action::Update, Resource::Database),
				],
				Step::RemoveVariable { .. } => {
					vec![
						(Action::Read, Resource::EnvVar),
						(Action::Delete, Resource::EnvVar),
					]
				}
				Step::LinkModel { .. } => vec![
					(Action::Update, Resource::Application),
					(Action::Read, Resource::Model),
				],
				Step::UnlinkModel { .. } => vec![(Action::Update, Resource::Application)],
				Step::Redeploy { node } => match node.kind {
					ResourceKind::Stack => vec![(Action::Update, Resource::Stack)],
					_ => vec![(Action::Create, Resource::Deployment)],
				},
			};
			for need in more {
				if !out.contains(&need) {
					out.push(need);
				}
			}
		}
		out
	}
}

const fn permission(kind: ResourceKind) -> Resource {
	match kind {
		ResourceKind::Application => Resource::Application,
		ResourceKind::Database => Resource::Database,
		ResourceKind::Model => Resource::Model,
		ResourceKind::Stack => Resource::Stack,
	}
}

fn refuse<T>(text: impl Into<String>) -> Result<T, Refusal> {
	Err(Refusal(text.into()))
}

/// The name of an environment's own network, as the lists show it.
fn env_network(graph: &Graph, node: &Node) -> String {
	let _ = graph;
	crate::networks::environment_podman_name(node.environment)
}

fn on_env_network(node: &Node) -> bool {
	node.nets.iter().any(|n| n.auto)
}

/// Whether two nodes already share a network, by name. An application, database or model
/// of one environment on one server is on its environment's network from its first deploy
/// (a resource not yet deployed will be); a stack only when it was attached to it.
fn shared_network(graph: &Graph, a: &Node, b: &Node) -> Option<String> {
	if let Some(net) = a
		.nets
		.iter()
		.find(|n| !n.auto && b.nets.iter().any(|m| m.id == n.id))
	{
		return Some(net.label.clone());
	}
	let a_env = a.key.kind != ResourceKind::Stack || on_env_network(a);
	let b_env = b.key.kind != ResourceKind::Stack || on_env_network(b);
	(a_env && b_env).then(|| env_network(graph, a))
}

/// Decides what wiring `a` to `b` would do, or why not. The order of the checks is the
/// order of the refusals a person meets: same node, environment, server, then the kinds.
pub fn check(graph: &Graph, a: &Node, b: &Node) -> Result<Intent, Refusal> {
	if a.key == b.key {
		return refuse("Pick a different resource");
	}
	if a.environment != b.environment {
		let env = graph
			.environment(b.environment)
			.map_or("another environment", |e| e.name.as_str());
		return refuse(format!(
			"{} is in {env}. Wiring stays inside one environment",
			b.name
		));
	}
	// The arrow always points at the provider: a database or a model dropped on an
	// application is the same wire, flipped.
	let (a, b) = if !a.is_consumer() && b.is_consumer() {
		(b, a)
	} else {
		(a, b)
	};
	if a.server != b.server {
		return refuse(format!(
			"{} runs on another server than {}. Wiring needs both on one server",
			b.name, a.name
		));
	}
	match (a.key.kind, b.key.kind) {
		(ResourceKind::Model, _) => refuse("A model can only be linked from an app"),
		(_, ResourceKind::Model) => {
			if a.key.kind != ResourceKind::Application {
				return refuse("Only apps can use a model");
			}
			if graph
				.edges
				.iter()
				.any(|e| e.kind == EdgeKind::Gateway && e.from == a.key && e.to == b.key)
			{
				return refuse("Already linked through the model gateway");
			}
			Ok(Intent::Gateway {
				app: a.key,
				model: b.key,
			})
		}
		(ResourceKind::Application, ResourceKind::Database) => {
			if let Some(e) = graph
				.edges
				.iter()
				.find(|e| e.kind == EdgeKind::Variable && e.from == a.key && e.to == b.key)
			{
				return refuse(format!("Already connected through {}", e.label));
			}
			Ok(Intent::Variable {
				consumer: a.key,
				provider: b.key,
			})
		}
		_ => {
			if let Some(net) = shared_network(graph, a, b) {
				return refuse(format!("Already on network {net}"));
			}
			let join: Vec<NodeKey> = [a, b]
				.into_iter()
				.filter(|n| n.key.kind == ResourceKind::Stack && !on_env_network(n))
				.map(|n| n.key)
				.collect();
			Ok(Intent::Network {
				a: a.key,
				b: b.key,
				join,
				network: env_network(graph, a),
			})
		}
	}
}

/// Everything `node` may be wired to, in graph order.
#[must_use]
pub fn targets(graph: &Graph, node: &Node) -> Vec<NodeKey> {
	graph
		.nodes
		.iter()
		.filter(|other| check(graph, node, other).is_ok())
		.map(|other| other.key)
		.collect()
}

fn ran(node: &Node) -> bool {
	!matches!(node.status, ResourceStatus::NotDeployed)
}

/// The variable name a consumer gets for a database: the engine's usual one, or one named
/// after the database when the consumer already has that name for something else.
#[must_use]
pub fn variable_key(consumer: &Node, provider: &Node) -> String {
	let usual = provider
		.var
		.clone()
		.unwrap_or_else(|| "DATABASE_URL".to_owned());
	if !consumer.env_keys.contains(&usual) {
		return usual;
	}
	let name: String = provider
		.name
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() {
				c.to_ascii_uppercase()
			} else {
				'_'
			}
		})
		.collect();
	format!("{}_URL", name.trim_matches('_'))
}

/// The ordered changes behind an [`Intent`].
#[must_use]
pub fn plan_of(graph: &Graph, intent: Intent) -> Plan {
	let mut steps = Vec::new();
	match &intent {
		Intent::Variable { consumer, provider } => {
			if let (Some(c), Some(p)) = (graph.node(*consumer), graph.node(*provider)) {
				steps.push(Step::SetVariable {
					node: *consumer,
					key: variable_key(c, p),
				});
				if ran(c) {
					steps.push(Step::Redeploy { node: *consumer });
				}
			}
		}
		Intent::Gateway { app, model } => {
			steps.push(Step::LinkModel {
				app: *app,
				model: *model,
			});
			if graph.node(*app).is_some_and(ran) {
				steps.push(Step::Redeploy { node: *app });
			}
		}
		Intent::Network { join, network, .. } => {
			for node in join {
				steps.push(Step::JoinNetwork {
					node: *node,
					network: network.clone(),
				});
			}
			for node in join {
				if graph.node(*node).is_some_and(ran) {
					steps.push(Step::Redeploy { node: *node });
				}
			}
		}
	}
	Plan { intent, steps }
}

/// The plan for wiring `from` to `to`.
pub fn plan(graph: &Graph, from: NodeKey, to: NodeKey) -> Result<Plan, Refusal> {
	let (Some(a), Some(b)) = (graph.node(from), graph.node(to)) else {
		return refuse("That resource is not in this project");
	};
	check(graph, a, b).map(|intent| plan_of(graph, intent))
}

/// The plan for undoing the link `edge`, the reverse of what wiring it did.
pub fn unplan(graph: &Graph, edge: &super::Edge) -> Result<Plan, Refusal> {
	if !graph.edges.contains(edge) {
		return refuse("That link no longer exists");
	}
	let mut steps = Vec::new();
	let intent = match edge.kind {
		EdgeKind::Variable => {
			steps.push(Step::RemoveVariable {
				node: edge.from,
				key: edge.label.clone(),
			});
			Intent::Variable {
				consumer: edge.from,
				provider: edge.to,
			}
		}
		EdgeKind::Gateway => {
			steps.push(Step::UnlinkModel {
				app: edge.from,
				model: edge.to,
			});
			Intent::Gateway {
				app: edge.from,
				model: edge.to,
			}
		}
		EdgeKind::Network => {
			steps.push(Step::LeaveNetwork {
				node: edge.from,
				network: edge.label.clone(),
			});
			Intent::Network {
				a: edge.from,
				b: edge.to,
				join: Vec::new(),
				network: edge.label.clone(),
			}
		}
	};
	if edge.kind != EdgeKind::Network && graph.node(edge.from).is_some_and(ran) {
		steps.push(Step::Redeploy { node: edge.from });
	}
	Ok(Plan { intent, steps })
}
