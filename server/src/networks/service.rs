//! Network use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then touch the database or the agent.
//!
//! Networks belong to a server, so they reuse the `server` permissions:
//! `read` to look, `update` to create, rename, import, delete and migrate.
//! Attaching a resource needs `update` on the resource itself.

use axum::http::Method;
use proto::{NetworkCreate, NetworkInfo};

use super::{
	Member, Network, ResourceKind, find, importable_name, list_by_environment, list_by_server,
	member_count, members, memberships, remove_member, slugify, upsert_member, valid_alias,
};
use crate::agents::AgentClient;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db;
use crate::error::{AppResult, ServerError};
use crate::ids::{EnvironmentId, NetworkId, ServerId, TeamId, UserId};
use crate::servers::Server;
use crate::state::AppState;
use crate::{scope, validate};

const MAX_NAME_LEN: usize = 60;
const SLUG_UNIQUE: &str = "networks_server_id_slug_key";
const PODMAN_UNIQUE: &str = "networks_server_id_podman_name_key";

/// A network with the resources on it.
#[derive(Debug, Clone)]
pub struct NetworkView {
	pub network: Network,
	pub members: Vec<Member>,
}

/// A resource, as far as networks care.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ResourceRef {
	pub id: i64,
	pub name: String,
	pub project_id: i64,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
}

/// What a resource's Networks tab shows.
#[derive(Debug, Clone)]
pub struct ResourceNetworks {
	pub resource: ResourceRef,
	pub kind: ResourceKind,
	/// The networks the resource is on, its environment's first.
	pub joined: Vec<(Network, Vec<String>)>,
	/// Networks of the same server it could be attached to.
	pub available: Vec<Network>,
}

fn validate_name(name: &str) -> AppResult<(String, String)> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	let slug = slugify(name).ok_or_else(|| {
		ServerError::Validation("Name needs at least one letter or digit.".to_owned())
	})?;
	debug_assert!(validate::slug(&slug));
	Ok((name.to_owned(), slug))
}

fn name_conflict(e: sqlx::Error) -> ServerError {
	if db::is_unique_violation(&e, SLUG_UNIQUE) {
		ServerError::Conflict("A network with that name already exists on this server.".to_owned())
	} else if db::is_unique_violation(&e, PODMAN_UNIQUE) {
		ServerError::Conflict("That network is already registered on this server.".to_owned())
	} else {
		e.into()
	}
}

async fn network(state: &AppState, team: TeamId, id: NetworkId) -> AppResult<Network> {
	find(&state.db, team, id)
		.await?
		.ok_or(ServerError::NotFound)
}

async fn client(state: &AppState, server: &Server) -> AppResult<AgentClient> {
	state.agents.client(state, server).await
}

/// The networks of a server with their members; needs `read server`.
pub async fn list(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Vec<NetworkView>> {
	authorize(&state.db, user, team, Action::Read, Resource::Server).await?;
	scope::server(&state.db, team, server).await?;
	let networks = list_by_server(&state.db, team, server).await?;
	let ids: Vec<NetworkId> = networks.iter().map(|n| n.id).collect();
	let all = members(&state.db, &ids).await?;
	Ok(networks
		.into_iter()
		.map(|network| NetworkView {
			members: all
				.iter()
				.filter(|m| m.network_id == network.id)
				.cloned()
				.collect(),
			network,
		})
		.collect())
}

/// One network with its members; needs `read server`.
pub async fn get(
	state: &AppState,
	user: UserId,
	team: TeamId,
	id: NetworkId,
) -> AppResult<NetworkView> {
	authorize(&state.db, user, team, Action::Read, Resource::Server).await?;
	let network = network(state, team, id).await?;
	let members = members(&state.db, &[network.id]).await?;
	Ok(NetworkView { network, members })
}

/// The networks of an environment (one per server it runs on); needs
/// `read project`.
pub async fn for_environment(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: crate::ids::ProjectId,
	environment: EnvironmentId,
) -> AppResult<Vec<NetworkView>> {
	authorize(&state.db, user, team, Action::Read, Resource::Project).await?;
	scope::environment(&state.db, team, project, environment).await?;
	let networks = list_by_environment(&state.db, team, environment).await?;
	let ids: Vec<NetworkId> = networks.iter().map(|n| n.id).collect();
	let all = members(&state.db, &ids).await?;
	Ok(networks
		.into_iter()
		.map(|network| NetworkView {
			members: all
				.iter()
				.filter(|m| m.network_id == network.id)
				.cloned()
				.collect(),
			network,
		})
		.collect())
}

/// Makes a network on the server; needs `update server`. With `internal`,
/// containers on it cannot reach the outside.
pub async fn create(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	name: &str,
	internal: bool,
) -> AppResult<Network> {
	authorize(&state.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&state.db, team, server).await?;
	let (name, slug) = validate_name(name)?;
	let id = NetworkId::generate();
	let podman_name = format!("{}{id}", proto::USER_NETWORK_PREFIX);

	let row: Network = sqlx::query_as(
		"INSERT INTO networks (id, team_id, server_id, name, slug, kind, podman_name, internal)
		VALUES ($1, $2, $3, $4, $5, 'user', $6, $7)
		RETURNING id, team_id, server_id, name, slug, kind, environment_id, podman_name, internal,
			imported, created_at",
	)
	.bind(id)
	.bind(team)
	.bind(server.id)
	.bind(&name)
	.bind(&slug)
	.bind(&podman_name)
	.bind(internal)
	.fetch_one(&state.db)
	.await
	.map_err(name_conflict)?;

	let made = async {
		let agent = client(state, &server).await?;
		agent
			.json::<NetworkInfo>(
				Method::POST,
				"/v1/networks",
				Some(&NetworkCreate {
					name: podman_name.clone(),
					internal,
				}),
			)
			.await
	}
	.await;
	if let Err(e) = made {
		sqlx::query("DELETE FROM networks WHERE id = $1")
			.bind(id)
			.execute(&state.db)
			.await?;
		return Err(e);
	}
	audit::log(
		&state.db,
		user,
		team,
		"network.create",
		Target::new("network", row.id).named(&row.slug),
		Diff::new()
			.set("name", row.name.clone())
			.set("server_id", server.id.0)
			.set("internal", internal),
	)
	.await;
	Ok(row)
}

/// Renames a user network (the Podman name stays); needs `update server`.
pub async fn rename(
	state: &AppState,
	user: UserId,
	team: TeamId,
	id: NetworkId,
	name: &str,
) -> AppResult<Network> {
	authorize(&state.db, user, team, Action::Update, Resource::Server).await?;
	let current = network(state, team, id).await?;
	if !current.is_user() {
		return Err(ServerError::Validation(
			"Only networks you made or imported can be renamed.".to_owned(),
		));
	}
	let (name, _) = validate_name(name)?;
	sqlx::query("UPDATE networks SET name = $2 WHERE id = $1")
		.bind(current.id)
		.bind(&name)
		.execute(&state.db)
		.await?;
	audit::log(
		&state.db,
		user,
		team,
		"network.rename",
		Target::new("network", current.id).named(&current.slug),
		Diff::new().change("name", current.name.clone(), name),
	)
	.await;
	network(state, team, id).await
}

/// Removes a user network; needs `update server`. Refused while any resource
/// is on it. An imported network is only forgotten, never removed from Podman.
pub async fn delete(state: &AppState, user: UserId, team: TeamId, id: NetworkId) -> AppResult<()> {
	authorize(&state.db, user, team, Action::Update, Resource::Server).await?;
	let current = network(state, team, id).await?;
	if !current.is_user() {
		return Err(ServerError::Validation(
			"An environment's network goes with the environment.".to_owned(),
		));
	}
	let count = member_count(&state.db, current.id).await?;
	if count > 0 {
		return Err(ServerError::Conflict(format!(
			"{} still on this network ({count}). Detach {} first.",
			if count == 1 {
				"A resource is"
			} else {
				"Resources are"
			},
			if count == 1 { "it" } else { "them" },
		)));
	}
	if !current.imported {
		let server = scope::server(&state.db, team, current.server_id).await?;
		client(state, &server)
			.await?
			.send(
				Method::DELETE,
				&format!("/v1/networks/{}", current.podman_name),
			)
			.await?;
	}
	sqlx::query("DELETE FROM networks WHERE id = $1")
		.bind(current.id)
		.execute(&state.db)
		.await?;
	audit::log(
		&state.db,
		user,
		team,
		"network.delete",
		Target::new("network", current.id).named(&current.slug),
		Diff::new()
			.set("name", current.name.clone())
			.set("imported", current.imported),
	)
	.await;
	Ok(())
}

/// The Podman networks of a server that podkit does not manage yet; needs
/// `read server`.
pub async fn scan(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Vec<NetworkInfo>> {
	authorize(&state.db, user, team, Action::Read, Resource::Server).await?;
	let server = scope::server(&state.db, team, server).await?;
	let known: Vec<String> =
		sqlx::query_scalar("SELECT podman_name FROM networks WHERE server_id = $1")
			.bind(server.id)
			.fetch_all(&state.db)
			.await?;
	let all: Vec<NetworkInfo> = client(state, &server)
		.await?
		.json(Method::GET, "/v1/networks", None::<&()>)
		.await?;
	Ok(all
		.into_iter()
		.filter(|n| importable_name(&n.name) && !known.contains(&n.name))
		.collect())
}

/// Registers a network that already exists on the server; needs
/// `update server`. The network must be there: the agent is asked.
pub async fn import(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	podman_name: &str,
	name: Option<&str>,
) -> AppResult<Network> {
	authorize(&state.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&state.db, team, server).await?;
	let podman_name = podman_name.trim();
	if !importable_name(podman_name) {
		return Err(ServerError::Validation(
			"That network cannot be imported: system, host and podkit-made networks are managed by podkit."
				.to_owned(),
		));
	}
	let info: NetworkInfo = client(state, &server)
		.await?
		.json(
			Method::GET,
			&format!("/v1/networks/{podman_name}"),
			None::<&()>,
		)
		.await
		.map_err(|e| match e {
			ServerError::Validation(_) => ServerError::Validation(format!(
				"There is no network named \"{podman_name}\" on this server."
			)),
			other => other,
		})?;
	let (name, slug) = validate_name(name.filter(|n| !n.trim().is_empty()).unwrap_or(&info.name))?;
	let imported: Network = sqlx::query_as(
		"INSERT INTO networks (id, team_id, server_id, name, slug, kind, podman_name, internal, imported)
		VALUES ($1, $2, $3, $4, $5, 'user', $6, $7, TRUE)
		RETURNING id, team_id, server_id, name, slug, kind, environment_id, podman_name, internal,
			imported, created_at",
	)
	.bind(NetworkId::generate())
	.bind(team)
	.bind(server.id)
	.bind(name)
	.bind(slug)
	.bind(&info.name)
	.bind(info.internal)
	.fetch_one(&state.db)
	.await
	.map_err(name_conflict)?;
	audit::log(
		&state.db,
		user,
		team,
		"network.import",
		Target::new("network", imported.id).named(&imported.slug),
		Diff::new()
			.set("name", imported.name.clone())
			.set("podman_name", imported.podman_name.clone())
			.set("server_id", server.id.0),
	)
	.await;
	Ok(imported)
}

/// A resource of `team`; another team's id is the resource's not-found.
pub async fn resource(
	state: &AppState,
	team: TeamId,
	kind: ResourceKind,
	id: i64,
) -> AppResult<ResourceRef> {
	let table = match kind {
		ResourceKind::Application => "applications",
		ResourceKind::Database => "databases",
		ResourceKind::Model => "models",
		ResourceKind::Stack => "stacks",
	};
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT r.id, r.name, r.project_id, r.environment_id, r.server_id
		FROM {table} r JOIN projects p ON p.id = r.project_id
		WHERE r.id = $1 AND p.team_id = $2"
	)))
	.bind(id)
	.bind(team)
	.fetch_optional(&state.db)
	.await?
	.ok_or_else(|| kind.not_found())
}

/// The Networks tab of a resource; needs `read` on the resource.
pub async fn for_resource(
	state: &AppState,
	user: UserId,
	team: TeamId,
	kind: ResourceKind,
	id: i64,
) -> AppResult<ResourceNetworks> {
	authorize(&state.db, user, team, Action::Read, kind.permission()).await?;
	let resource = resource(state, team, kind, id).await?;
	let mut joined = memberships(&state.db, kind, id).await?;
	// A resource that was never deployed is not recorded on its environment's
	// network yet; it joins it with its first deploy.
	if kind != ResourceKind::Stack && !joined.iter().any(|(n, _)| n.is_environment()) {
		let env = super::ensure_environment(&state.db, resource.environment_id, resource.server_id)
			.await?;
		joined.insert(0, (env, Vec::new()));
	}
	let on: Vec<NetworkId> = joined.iter().map(|(n, _)| n.id).collect();
	let available = list_by_server(&state.db, team, resource.server_id)
		.await?
		.into_iter()
		.filter(|n| !on.contains(&n.id))
		.filter(|n| {
			n.is_user()
				|| (kind == ResourceKind::Stack
					&& n.environment_id == Some(resource.environment_id))
		})
		.collect();
	Ok(ResourceNetworks {
		resource,
		kind,
		joined,
		available,
	})
}

/// Puts a resource on a network under `aliases` (its name when none are
/// given); needs `update` on the resource. Takes effect on the resource's
/// next deploy. A stack may also be attached to its environment's network;
/// every other resource is on that one already.
pub async fn attach(
	state: &AppState,
	user: UserId,
	team: TeamId,
	network_id: NetworkId,
	kind: ResourceKind,
	id: i64,
	aliases: &[String],
) -> AppResult<()> {
	authorize(&state.db, user, team, Action::Update, kind.permission()).await?;
	let resource = resource(state, team, kind, id).await?;
	let network = network(state, team, network_id).await?;
	if network.server_id != resource.server_id {
		return Err(ServerError::Validation(
			"The network and the resource are on different servers.".to_owned(),
		));
	}
	if network.is_environment()
		&& !(kind == ResourceKind::Stack && network.environment_id == Some(resource.environment_id))
	{
		return Err(ServerError::Validation(
			"Only a stack of that environment can be attached to its network.".to_owned(),
		));
	}
	let aliases = if aliases.is_empty() {
		vec![slugify(&resource.name).unwrap_or_else(|| format!("{}-{id}", kind.as_str()))]
	} else {
		aliases.to_vec()
	};
	if aliases.len() > 8 || aliases.iter().any(|a| !valid_alias(a)) {
		return Err(ServerError::Validation(
			"Aliases are up to 8 names of lowercase letters, digits, dots, hyphens and underscores."
				.to_owned(),
		));
	}
	upsert_member(&state.db, network.id, kind, id, &aliases).await?;
	audit::log(
		&state.db,
		user,
		team,
		"network.attach",
		Target::new("network", network.id).named(&network.slug),
		Diff::new()
			.set("resource_kind", kind.as_str())
			.set("resource_id", id)
			.set("aliases", aliases.clone()),
	)
	.await;
	Ok(())
}

/// Takes a resource off a user network (or a stack off its environment's);
/// needs `update` on the resource. Takes effect on its next deploy.
pub async fn detach(
	state: &AppState,
	user: UserId,
	team: TeamId,
	network_id: NetworkId,
	kind: ResourceKind,
	id: i64,
) -> AppResult<()> {
	authorize(&state.db, user, team, Action::Update, kind.permission()).await?;
	resource(state, team, kind, id).await?;
	let network = network(state, team, network_id).await?;
	if network.is_environment() && kind != ResourceKind::Stack {
		return Err(ServerError::Validation(
			"A resource always stays on its environment's network.".to_owned(),
		));
	}
	if !remove_member(&state.db, network.id, kind, id).await? {
		return Err(ServerError::NotFound);
	}
	audit::log(
		&state.db,
		user,
		team,
		"network.detach",
		Target::new("network", network.id).named(&network.slug),
		Diff::new()
			.set("resource_kind", kind.as_str())
			.set("resource_id", id),
	)
	.await;
	Ok(())
}

/// Splits `podkit-<app|db|model>-<id>-<instance>` into the resource.
#[must_use]
pub fn parse_container(name: &str) -> Option<(ResourceKind, i64)> {
	let rest = name.strip_prefix("podkit-")?;
	let (kind, rest) = if let Some(r) = rest.strip_prefix("app-") {
		(ResourceKind::Application, r)
	} else if let Some(r) = rest.strip_prefix("db-") {
		(ResourceKind::Database, r)
	} else {
		(ResourceKind::Model, rest.strip_prefix("model-")?)
	};
	let (id, instance) = rest.split_once('-')?;
	instance.bytes().all(|b| b.is_ascii_digit()).then_some(())?;
	Some((kind, id.parse().ok()?))
}

/// The aliases an app, database or model answers to on its environment
/// network: what its deploy asks for.
#[must_use]
pub fn default_aliases(kind: ResourceKind, id: i64, slug: Option<&str>) -> Vec<String> {
	match kind {
		ResourceKind::Database => vec![format!("podkit-db-{id}")],
		ResourceKind::Model => slug.map(crate::models::alias).into_iter().collect(),
		_ => Vec::new(),
	}
}
