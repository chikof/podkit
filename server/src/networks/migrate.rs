//! The one-shot "migrate networks" run of a server: moves the containers that
//! run today on the shared `podkit` network onto their environment's network
//! without restarting them (`podman network connect` before `disconnect`).
//! Containers of compose stacks are left alone; they move on their next
//! deploy. Anything not migrated moves on its next deploy anyway.

use axum::http::Method;
use proto::{ContainerInfo, ContainerState, Ingress, NetworkMember};
use tracing::warn;

use super::attach::{self, Home};
use super::service::{default_aliases, parse_container, resource};
use super::{ResourceKind, importable_name};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::AppResult;
use crate::ids::{ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::{links, scope};

/// What a migration did, by container name.
#[derive(Debug, Default, Clone)]
pub struct Report {
	/// Moved to its environment network, without a restart.
	pub moved: Vec<String>,
	/// Needs a redeploy to move (stacks, and containers that could not be moved).
	pub redeploy: Vec<String>,
}

/// Migrates every running app, database and model container of the server;
/// needs `update server`.
pub async fn run(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Report> {
	authorize(&state.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&state.db, team, server).await?;
	let client = state.agents.client(state, &server).await?;
	let mut report = Report::default();

	// A Traefik from before per-network routing is replaced first, so it
	// reaches the containers once they leave `podkit`.
	let ingress = Ingress {
		http_port: state.config.ingress_port,
		https_port: state.config.https_port,
		acme_email: state.config.acme_email.clone(),
	};
	client
		.send_json(Method::POST, "/v1/ingress/ensure", &ingress)
		.await?;

	let containers: Vec<ContainerInfo> = client
		.json(Method::GET, "/v1/containers", None::<&()>)
		.await?;
	for container in containers
		.into_iter()
		.filter(|c| c.state == ContainerState::Running)
	{
		if container.name.starts_with("podkit-stack-") {
			report.redeploy.push(container.name);
			continue;
		}
		let Some((kind, id)) = parse_container(&container.name) else {
			continue;
		};
		let moved = move_one(state, &client, team, server.id, kind, id, &container.name).await;
		match moved {
			Ok(true) => report.moved.push(container.name),
			Ok(false) => {}
			Err(e) => {
				warn!(container = %container.name, error = %e, "moving a container failed");
				report.redeploy.push(container.name);
			}
		}
	}
	if let Err(e) = links::push(state, &client, server.id).await {
		warn!(server = %server.id, error = %e, "pushing the gateway networks failed");
	}
	audit::log(
		&state.db,
		user,
		team,
		"server.networks_migrate",
		Target::new("server", server.id).named(&server.name),
		Diff::new()
			.set("moved", report.moved.len())
			.set("redeploy", report.redeploy.len()),
	)
	.await;
	Ok(report)
}

/// `Ok(false)` when the container is not one of this server's resources.
async fn move_one(
	state: &AppState,
	client: &crate::agents::AgentClient,
	team: TeamId,
	server: ServerId,
	kind: ResourceKind,
	id: i64,
	container: &str,
) -> AppResult<bool> {
	let Ok(resource) = resource(state, team, kind, id).await else {
		return Ok(false);
	};
	if resource.server_id != server {
		return Ok(false);
	}
	let slug: Option<String> = if kind == ResourceKind::Model {
		sqlx::query_scalar("SELECT slug FROM models WHERE id = $1")
			.bind(id)
			.fetch_optional(&state.db)
			.await?
	} else {
		None
	};
	let aliases = default_aliases(kind, id, slug.as_deref());
	let home = Home {
		kind,
		id,
		environment: resource.environment_id,
		server,
	};
	for network in attach::for_resource(&state.db, home, &aliases).await? {
		if !importable_name(&network.name) {
			client
				.send_json(
					Method::POST,
					"/v1/networks",
					&proto::NetworkCreate {
						name: network.name.clone(),
						internal: network.internal,
					},
				)
				.await?;
		}
		client
			.send_json(
				Method::POST,
				&format!("/v1/networks/{}/connect", network.name),
				&NetworkMember {
					container: container.to_owned(),
					aliases: network.aliases.clone(),
				},
			)
			.await?;
	}
	// Only now that it is reachable on its own network.
	client
		.send_json(
			Method::POST,
			&format!("/v1/networks/{}/disconnect", proto::SYSTEM_NETWORK),
			&NetworkMember {
				container: container.to_owned(),
				aliases: Vec::new(),
			},
		)
		.await?;
	Ok(true)
}
