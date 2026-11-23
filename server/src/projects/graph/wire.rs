//! Applying a wire (and undoing one) through the services that already own each change:
//! network attach (N1), env vars, the model link (AI 05) and deploys. The canvas has no
//! write path of its own; this only orders them and checks, before the first change, that
//! the caller may make every one.

use super::rules::{self, Plan, Step};
use super::{Edge, Graph, Node, NodeKey, build};
use crate::auth::permission::authorize;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, DatabaseId, ModelId, NetworkId, ProjectId, StackId, TeamId, UserId,
};
use crate::projects::overview::ResourceKind;
use crate::state::AppState;
use crate::{databases, deployments, env_vars, links, networks, stacks};

/// What a plan changed, for the confirmation toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
	pub redeployed: Vec<String>,
	/// The consumer and the provider, after any reversal.
	pub ends: (NodeKey, NodeKey),
}

fn node(graph: &Graph, key: NodeKey) -> AppResult<&Node> {
	// A resource of another team, or of another project, is simply not in this graph.
	graph.node(key).ok_or(ServerError::NotFound)
}

/// Every permission the plan needs, before anything changes.
async fn allowed(s: &AppState, user: UserId, team: TeamId, plan: &Plan) -> AppResult<()> {
	for (action, resource) in plan.needs() {
		authorize(&s.db, user, team, action, resource).await?;
	}
	Ok(())
}

/// The plan for wiring `from` to `to`, checked against the caller's permissions: what the
/// popover shows and what the POST will run.
///
/// # Errors
/// `NotFound` for a resource that is not in the project (another team's included),
/// `Validation` with the reason for a refused wire, `Forbidden` for a missing permission.
pub async fn preview(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	from: NodeKey,
	to: NodeKey,
) -> AppResult<(Graph, Plan)> {
	let graph = build(s, user, team, project).await?;
	node(&graph, from)?;
	node(&graph, to)?;
	let plan = rules::plan(&graph, from, to).map_err(|r| ServerError::Validation(r.0))?;
	allowed(s, user, team, &plan).await?;
	Ok((graph, plan))
}

/// The plan for removing `edge`, checked like [`preview`].
///
/// # Errors
/// As [`preview`].
pub async fn unpreview(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	edge: &Edge,
) -> AppResult<(Graph, Plan)> {
	let graph = build(s, user, team, project).await?;
	node(&graph, edge.from)?;
	node(&graph, edge.to)?;
	let plan = rules::unplan(&graph, edge).map_err(|r| ServerError::Validation(r.0))?;
	allowed(s, user, team, &plan).await?;
	Ok((graph, plan))
}

/// Wires `from` to `to`: re-validates, then runs the plan.
///
/// # Errors
/// As [`preview`], plus whatever a service refuses on the way.
pub async fn wire(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	from: NodeKey,
	to: NodeKey,
) -> AppResult<Applied> {
	let (graph, plan) = preview(s, user, team, project, from, to).await?;
	run(s, user, team, project, &graph, &plan).await
}

/// Removes the link `edge`.
///
/// # Errors
/// As [`unpreview`].
pub async fn unwire(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	edge: &Edge,
) -> AppResult<Applied> {
	let (graph, plan) = unpreview(s, user, team, project, edge).await?;
	run(s, user, team, project, &graph, &plan).await
}

#[allow(
	clippy::too_many_lines,
	reason = "one flat match over the steps reads better than six helpers"
)]
async fn run(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	graph: &Graph,
	plan: &Plan,
) -> AppResult<Applied> {
	let mut redeployed = Vec::new();
	for step in &plan.steps {
		match step {
			Step::JoinNetwork { node: key, .. } => {
				let n = node(graph, *key)?;
				let net = networks::ensure_environment(&s.db, n.environment, n.server).await?;
				networks::service::attach(s, user, team, net.id, net_kind(*key), key.id, &[])
					.await?;
			}
			Step::LeaveNetwork { node: key, network } => {
				let n = node(graph, *key)?;
				let net = n
					.nets
					.iter()
					.find(|m| !m.auto && &m.label == network)
					.ok_or(ServerError::NotFound)?;
				networks::service::detach(
					s,
					user,
					team,
					NetworkId(net.id.0),
					net_kind(*key),
					key.id,
				)
				.await?;
			}
			Step::SetVariable {
				node: key,
				key: name,
			} => {
				let (consumer, provider) = plan.ends();
				debug_assert_eq!(consumer, *key);
				let connection = crate::audit::quiet(databases::service::reveal(
					s,
					user,
					team,
					project,
					DatabaseId(provider.id),
				))
				.await?;
				env_vars::service::upsert(
					s,
					user,
					team,
					project,
					ApplicationId(key.id),
					name,
					&connection.internal_url,
				)
				.await?;
			}
			Step::RemoveVariable {
				node: key,
				key: name,
			} => {
				let vars =
					env_vars::service::list(&s.db, user, team, project, ApplicationId(key.id))
						.await?;
				let id = vars
					.iter()
					.find(|v| &v.key == name)
					.map(|v| v.id)
					.ok_or(ServerError::EnvVarNotFound)?;
				env_vars::service::remove(&s.db, user, team, project, ApplicationId(key.id), id)
					.await?;
			}
			Step::LinkModel { app, model } => {
				links::service::link(
					s,
					user,
					team,
					project,
					ApplicationId(app.id),
					ModelId(model.id),
					false,
				)
				.await?;
			}
			Step::UnlinkModel { app, model } => {
				links::service::unlink(
					s,
					user,
					team,
					project,
					ApplicationId(app.id),
					ModelId(model.id),
				)
				.await?;
			}
			Step::Redeploy { node: key } => {
				match key.kind {
					ResourceKind::Stack => {
						stacks::service::deploy(s, user, team, project, StackId(key.id)).await?;
					}
					_ => {
						deployments::service::deploy(s, user, team, project, ApplicationId(key.id))
							.await?;
					}
				}
				redeployed.push(node(graph, *key)?.name.clone());
			}
		}
	}
	Ok(Applied {
		redeployed,
		ends: plan.ends(),
	})
}

fn net_kind(key: NodeKey) -> networks::ResourceKind {
	match key.kind {
		ResourceKind::Application => networks::ResourceKind::Application,
		ResourceKind::Database => networks::ResourceKind::Database,
		ResourceKind::Model => networks::ResourceKind::Model,
		ResourceKind::Stack => networks::ResourceKind::Stack,
	}
}
