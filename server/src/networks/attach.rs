//! The networks a deploy puts a container on.

use proto::NetworkAttach;
use sqlx::PgPool;

use super::{ResourceKind, ensure_environment, memberships, upsert_member};
use crate::ids::{EnvironmentId, ServerId, StackId};

/// Where a resource lives.
#[derive(Debug, Clone, Copy)]
pub struct Home {
	pub kind: ResourceKind,
	pub id: i64,
	pub environment: EnvironmentId,
	pub server: ServerId,
}

/// The networks an app, database or model container joins: its environment's
/// network first (where Traefik reaches it, answering to `aliases`), then the
/// user networks it was attached to, each under the aliases chosen there.
///
/// The environment network is made on first use and the resource is recorded
/// as a member of it.
///
/// # Errors
/// The database fails.
pub async fn for_resource(
	db: &PgPool,
	home: Home,
	aliases: &[String],
) -> Result<Vec<NetworkAttach>, sqlx::Error> {
	let env = ensure_environment(db, home.environment, home.server).await?;
	upsert_member(db, env.id, home.kind, home.id, aliases).await?;
	let mut out = vec![NetworkAttach {
		name: env.podman_name,
		aliases: aliases.to_vec(),
		internal: env.internal,
	}];
	for (network, member_aliases) in memberships(db, home.kind, home.id).await? {
		if network.is_environment() || network.server_id != home.server {
			continue;
		}
		out.push(NetworkAttach {
			name: network.podman_name,
			aliases: member_aliases,
			internal: network.internal,
		});
	}
	Ok(out)
}

/// A network a stack was attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
	pub name: String,
	pub internal: bool,
}

/// The networks beyond its own that a stack's services join: the ones it was
/// attached to, and only those. A stack is not on its environment's network
/// unless it was attached to it.
///
/// # Errors
/// The database fails.
pub async fn stack_links(db: &PgPool, stack: StackId) -> Result<Vec<Linked>, sqlx::Error> {
	Ok(memberships(db, ResourceKind::Stack, stack.0)
		.await?
		.into_iter()
		.map(|(n, _)| Linked {
			name: n.podman_name,
			internal: n.internal,
		})
		.collect())
}
