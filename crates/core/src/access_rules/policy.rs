//! The network policy view and the exposure warnings.
//!
//! Containers on one network reach each other by name and nothing else does (N1), so the
//! policy of an environment follows from who is a member of which network: everybody on a
//! network can reach everybody else on it. What this adds is the other direction, who can be
//! reached from outside the server: an application through the ingress (and whether its
//! access rules restrict that), or a database through a published host port, which skips the
//! ingress and every rule on it.

use std::collections::HashMap;

use sqlx::PgPool;

use crate::ids::{ServerId, TeamId};
use crate::networks::service::NetworkView;
use crate::networks::{Member, Network, ResourceKind};

/// How a resource can be reached from outside the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
	/// Through the ingress. `restricted`: an IP list, a rate or size limit or a login is set.
	Public { restricted: bool },
	/// Off the ingress by the internal-only switch (or an MCP server, which never is on it).
	InternalOnly,
	/// A host port that goes straight to the container, around the ingress and its rules.
	PublishedPort(i32),
	/// Reachable only from the networks it is on.
	Private,
}

/// A member of a network and how it is exposed.
#[derive(Debug, Clone)]
pub struct PolicyMember {
	pub member: Member,
	pub exposure: Exposure,
}

/// One network: its members can all reach each other.
#[derive(Debug, Clone)]
pub struct NetworkPolicy {
	pub network: Network,
	pub members: Vec<PolicyMember>,
}

/// A resource with a published host port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortWarning {
	pub kind: ResourceKind,
	pub id: i64,
	pub project_id: i64,
	pub name: String,
	pub port: i32,
}

/// What the Networks page shows.
#[derive(Debug, Clone, Default)]
pub struct Policy {
	pub networks: Vec<NetworkPolicy>,
	pub warnings: Vec<PortWarning>,
}

#[derive(sqlx::FromRow)]
struct AppExposure {
	id: i64,
	internal: bool,
	restricted: bool,
}

#[derive(sqlx::FromRow)]
struct DbPort {
	id: i64,
	project_id: i64,
	name: String,
	public_port: i32,
}

/// The policy of `views`, which the caller has already read through `networks::service`
/// (so they are the team's own), plus the warnings of `server`.
pub async fn build(
	db: &PgPool,
	team: TeamId,
	server: ServerId,
	views: Vec<NetworkView>,
) -> Result<Policy, sqlx::Error> {
	let apps: Vec<i64> = views
		.iter()
		.flat_map(|v| &v.members)
		.filter(|m| m.kind() == Some(ResourceKind::Application))
		.map(|m| m.resource_id)
		.collect();
	let exposures: HashMap<i64, AppExposure> = sqlx::query_as::<_, AppExposure>(
		"SELECT a.id, \
		 (COALESCE(r.internal_only, FALSE) OR EXISTS (SELECT 1 FROM mcp_servers m WHERE m.application_id = a.id)) AS internal, \
		 (COALESCE(cardinality(r.allow_cidrs), 0) + COALESCE(cardinality(r.deny_cidrs), 0) > 0 \
		  OR r.rate_average IS NOT NULL OR r.max_body_mb IS NOT NULL \
		  OR EXISTS (SELECT 1 FROM access_users u WHERE u.application_id = a.id)) AS restricted \
		 FROM applications a LEFT JOIN access_rules r ON r.application_id = a.id WHERE a.id = ANY($1)",
	)
	.bind(&apps)
	.fetch_all(db)
	.await?
	.into_iter()
	.map(|e| (e.id, e))
	.collect();

	let warnings = port_warnings(db, team, server).await?;
	let ports: HashMap<i64, i32> = warnings
		.iter()
		.filter(|w| w.kind == ResourceKind::Database)
		.map(|w| (w.id, w.port))
		.collect();

	let networks = views
		.into_iter()
		.map(|v| NetworkPolicy {
			members: v
				.members
				.into_iter()
				.map(|member| {
					let exposure = match member.kind() {
						Some(ResourceKind::Application) => match exposures.get(&member.resource_id)
						{
							Some(e) if e.internal => Exposure::InternalOnly,
							Some(e) => Exposure::Public {
								restricted: e.restricted,
							},
							None => Exposure::Private,
						},
						Some(ResourceKind::Database) => ports
							.get(&member.resource_id)
							.map_or(Exposure::Private, |p| Exposure::PublishedPort(*p)),
						_ => Exposure::Private,
					};
					PolicyMember { member, exposure }
				})
				.collect(),
			network: v.network,
		})
		.collect();
	Ok(Policy { networks, warnings })
}

/// The team's databases on `server` that publish a host port.
pub async fn port_warnings(
	db: &PgPool,
	team: TeamId,
	server: ServerId,
) -> Result<Vec<PortWarning>, sqlx::Error> {
	let rows: Vec<DbPort> = sqlx::query_as(
		"SELECT d.id, d.project_id, d.name, d.public_port FROM databases d \
		 JOIN projects p ON p.id = d.project_id \
		 WHERE d.server_id = $1 AND p.team_id = $2 AND d.public_port IS NOT NULL \
		 ORDER BY d.name, d.id",
	)
	.bind(server)
	.bind(team)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| PortWarning {
			kind: ResourceKind::Database,
			id: r.id,
			project_id: r.project_id,
			name: r.name,
			port: r.public_port,
		})
		.collect())
}
