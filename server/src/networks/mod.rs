//! Networks: every environment gets a private Podman network on each server
//! it runs resources on, so production and staging cannot reach each other.
//! Users add networks that connect resources on purpose, and can import a
//! network that already exists on the server.
//!
//! Podman's shared `podkit` network stays for system containers only
//! (Traefik and the gateway sidecar). Apps, databases and models join their
//! environment's network `podkit-env-<environment id>`; a compose stack keeps
//! its own network and joins others only when it is attached to them.

pub mod attach;
pub mod migrate;
mod routes;
pub mod service;

pub use routes::{network_router, router};

#[cfg(test)]
mod tests;

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::auth::permission::Resource;
use crate::error::ServerError;
use crate::ids::{EnvironmentId, NetworkId, ServerId, TeamId};

pub const KIND_ENVIRONMENT: &str = "environment";
pub const KIND_USER: &str = "user";

/// The Podman name of an environment's network.
#[must_use]
pub fn environment_podman_name(environment: EnvironmentId) -> String {
	format!("{}{}", proto::ENV_NETWORK_PREFIX, environment.0)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Network {
	pub id: NetworkId,
	pub server_id: ServerId,
	pub name: String,
	pub slug: String,
	/// `environment`, `user` or `system`.
	pub kind: String,
	pub environment_id: Option<EnvironmentId>,
	/// The name Podman knows it by.
	pub podman_name: String,
	/// No outbound access.
	pub internal: bool,
	/// Found on the server rather than made by podkit.
	pub imported: bool,
	pub created_at: OffsetDateTime,
}

impl Network {
	#[must_use]
	pub fn is_environment(&self) -> bool {
		self.kind == KIND_ENVIRONMENT
	}

	#[must_use]
	pub fn is_user(&self) -> bool {
		self.kind == KIND_USER
	}
}

/// The kinds of resource that sit on a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
	Application,
	Database,
	Model,
	Stack,
}

impl ResourceKind {
	pub const ALL: [Self; 4] = [Self::Application, Self::Database, Self::Model, Self::Stack];

	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Model => "model",
			Self::Stack => "stack",
		}
	}

	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|k| k.as_str() == s)
	}

	/// The permission resource that governs the kind.
	#[must_use]
	pub fn permission(self) -> Resource {
		match self {
			Self::Application => Resource::Application,
			Self::Database => Resource::Database,
			Self::Model => Resource::Model,
			Self::Stack => Resource::Stack,
		}
	}

	#[must_use]
	pub fn not_found(self) -> ServerError {
		match self {
			Self::Application => ServerError::ApplicationNotFound,
			Self::Database => ServerError::DatabaseNotFound,
			Self::Model => ServerError::ModelNotFound,
			Self::Stack => ServerError::StackNotFound,
		}
	}
}

/// A resource on a network, with what a page needs to link to it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Member {
	pub network_id: NetworkId,
	pub resource_kind: String,
	pub resource_id: i64,
	pub aliases: Vec<String>,
	pub name: String,
	pub project_id: i64,
}

impl Member {
	#[must_use]
	pub fn kind(&self) -> Option<ResourceKind> {
		ResourceKind::parse(&self.resource_kind)
	}
}

macro_rules! network_columns {
	() => {
		"n.id, n.server_id, n.name, n.slug, n.kind, n.environment_id, n.podman_name, \
		n.internal, n.imported, n.created_at"
	};
}

/// A network of `team`; another team's id is `None`, like a missing one.
pub async fn find(
	db: impl PgExecutor<'_>,
	team: TeamId,
	id: NetworkId,
) -> Result<Option<Network>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		network_columns!(),
		" FROM networks n WHERE n.id = $1 AND n.team_id = $2"
	))
	.bind(id)
	.bind(team)
	.fetch_optional(db)
	.await
}

/// The networks of a server: environment networks first, then by name.
pub async fn list_by_server(
	db: impl PgExecutor<'_>,
	team: TeamId,
	server: ServerId,
) -> Result<Vec<Network>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		network_columns!(),
		" FROM networks n WHERE n.server_id = $1 AND n.team_id = $2
		ORDER BY (n.kind <> 'environment'), n.name, n.id"
	))
	.bind(server)
	.bind(team)
	.fetch_all(db)
	.await
}

/// The networks of an environment, one per server it has resources on.
pub async fn list_by_environment(
	db: impl PgExecutor<'_>,
	team: TeamId,
	environment: EnvironmentId,
) -> Result<Vec<Network>, sqlx::Error> {
	sqlx::query_as(concat!(
		"SELECT ",
		network_columns!(),
		" FROM networks n
		WHERE n.environment_id = $1 AND n.team_id = $2 ORDER BY n.server_id"
	))
	.bind(environment)
	.bind(team)
	.fetch_all(db)
	.await
}

/// The environment's network on `server`, made on first use.
pub async fn ensure_environment(
	db: impl PgExecutor<'_>,
	environment: EnvironmentId,
	server: ServerId,
) -> Result<Network, sqlx::Error> {
	sqlx::query_as(
		"INSERT INTO networks (id, team_id, server_id, name, slug, kind, environment_id, podman_name)
		SELECT $1, p.team_id, $2, left(e.name || ' (' || p.name || ')', 60), 'env-' || e.id,
			'environment', e.id, 'podkit-env-' || e.id
		FROM environments e JOIN projects p ON p.id = e.project_id
		WHERE e.id = $3
		ON CONFLICT (environment_id, server_id) WHERE environment_id IS NOT NULL
		DO UPDATE SET internal = networks.internal
		RETURNING id, team_id, server_id, name, slug, kind, environment_id, podman_name, internal,
			imported, created_at",
	)
	.bind(NetworkId::generate())
	.bind(server)
	.bind(environment)
	.fetch_one(db)
	.await
}

/// The members of `networks`, with their names.
pub async fn members(
	db: impl PgExecutor<'_>,
	networks: &[NetworkId],
) -> Result<Vec<Member>, sqlx::Error> {
	let ids: Vec<i64> = networks.iter().map(|n| n.0).collect();
	sqlx::query_as(
		"SELECT rn.network_id, rn.resource_kind, rn.resource_id, rn.aliases,
			COALESCE(a.name, d.name, m.name, s.name, '') AS name,
			COALESCE(a.project_id, d.project_id, m.project_id, s.project_id, 0) AS project_id
		FROM resource_networks rn
		LEFT JOIN applications a ON rn.resource_kind = 'application' AND a.id = rn.resource_id
		LEFT JOIN databases d ON rn.resource_kind = 'database' AND d.id = rn.resource_id
		LEFT JOIN models m ON rn.resource_kind = 'model' AND m.id = rn.resource_id
		LEFT JOIN stacks s ON rn.resource_kind = 'stack' AND s.id = rn.resource_id
		WHERE rn.network_id = ANY($1)
		ORDER BY rn.resource_kind, name, rn.resource_id",
	)
	.bind(ids)
	.fetch_all(db)
	.await
}

/// How many resources sit on a network.
pub async fn member_count(db: impl PgExecutor<'_>, network: NetworkId) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar("SELECT COUNT(*) FROM resource_networks WHERE network_id = $1")
		.bind(network)
		.fetch_one(db)
		.await
}

/// Puts a resource on a network, replacing its aliases there.
pub async fn upsert_member(
	db: impl PgExecutor<'_>,
	network: NetworkId,
	kind: ResourceKind,
	resource: i64,
	aliases: &[String],
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO resource_networks (network_id, resource_kind, resource_id, aliases)
		VALUES ($1, $2, $3, $4)
		ON CONFLICT (network_id, resource_kind, resource_id) DO UPDATE SET aliases = EXCLUDED.aliases",
	)
	.bind(network)
	.bind(kind.as_str())
	.bind(resource)
	.bind(aliases)
	.execute(db)
	.await?;
	Ok(())
}

/// Takes a resource off a network; `false` when it was not on it.
pub async fn remove_member(
	db: impl PgExecutor<'_>,
	network: NetworkId,
	kind: ResourceKind,
	resource: i64,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query(
		"DELETE FROM resource_networks
		WHERE network_id = $1 AND resource_kind = $2 AND resource_id = $3",
	)
	.bind(network)
	.bind(kind.as_str())
	.bind(resource)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}

/// The networks a resource sits on, with its aliases there.
pub async fn memberships(
	db: impl PgExecutor<'_>,
	kind: ResourceKind,
	resource: i64,
) -> Result<Vec<(Network, Vec<String>)>, sqlx::Error> {
	let rows: Vec<(Network, Vec<String>)> = sqlx::query_as::<_, NetworkWithAliases>(concat!(
		"SELECT ",
		network_columns!(),
		", rn.aliases AS member_aliases
		FROM resource_networks rn JOIN networks n ON n.id = rn.network_id
		WHERE rn.resource_kind = $1 AND rn.resource_id = $2
		ORDER BY (n.kind <> 'environment'), n.name, n.id"
	))
	.bind(kind.as_str())
	.bind(resource)
	.fetch_all(db)
	.await?
	.into_iter()
	.map(|r| (r.network, r.member_aliases))
	.collect();
	Ok(rows)
}

#[derive(sqlx::FromRow)]
struct NetworkWithAliases {
	#[sqlx(flatten)]
	network: Network,
	member_aliases: Vec<String>,
}

/// A URL-safe slug from a free-form name: lowercase letters and digits,
/// runs of anything else becoming one hyphen. `None` when nothing is left.
#[must_use]
pub fn slugify(name: &str) -> Option<String> {
	let mut out = String::new();
	for c in name.trim().chars() {
		if c.is_ascii_alphanumeric() {
			out.push(c.to_ascii_lowercase());
		} else if !out.is_empty() && !out.ends_with('-') {
			out.push('-');
		}
	}
	let out = out.trim_end_matches('-');
	let out: String = out.chars().take(60).collect();
	let out = out.trim_end_matches('-').to_owned();
	(!out.is_empty()).then_some(out)
}

/// A DNS alias: 1 to 63 of `[a-z0-9._-]`.
#[must_use]
pub fn valid_alias(alias: &str) -> bool {
	(1..=63).contains(&alias.len())
		&& alias.bytes().all(|b| {
			b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
		})
}

/// Names no imported network may have: Podman's own and podkit's.
const RESERVED: [&str; 7] = [
	"host",
	"bridge",
	"none",
	"default",
	"podman",
	"pasta",
	"slirp4netns",
];

/// Whether a Podman network of this name may be imported: not a system,
/// host or podkit-made network.
#[must_use]
pub fn importable_name(name: &str) -> bool {
	!name.is_empty()
		&& name.len() <= 128
		&& name
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
		&& !RESERVED.contains(&name)
		&& name != proto::SYSTEM_NETWORK
		&& ![
			proto::ENV_NETWORK_PREFIX,
			proto::USER_NETWORK_PREFIX,
			proto::STACK_NETWORK_PREFIX,
		]
		.iter()
		.any(|p| name.starts_with(p))
}
