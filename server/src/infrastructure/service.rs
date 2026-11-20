//! Infrastructure use cases shared by the web UI: what runs where, and the setup
//! checklist. Authorize first, then scope (every query is by team), then read. A kind of
//! resource the caller may not read is left out, never an error, so a role that reads
//! servers and not databases sees a shorter picture.

use std::collections::HashMap;

use sqlx::PgPool;

use super::setup::{self, Item, Progress, Step, StepView, Visible};
use super::store::{self, Placed};
use crate::applications;
use crate::auth::permission::{Action, Permissions, Resource, authorize, granted};
use crate::error::{AppResult, ServerError};
use crate::ids::{ServerId, TeamId, UserId};
use crate::metrics::{Range, ServerPoint, service as metrics_service};
use crate::networks::service as network_service;
use crate::servers::{self, Server};
use crate::state::AppState;

/// How Traefik is set up, the same on every server.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
	pub http_port: u16,
	pub https_port: u16,
	pub acme_email: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NetworkLine {
	pub name: String,
	/// `environment`, `user` or `system`.
	pub kind: String,
	pub internal: bool,
	pub members: usize,
}

/// A resource of a server, with what the page needs to link to it.
#[derive(Debug, Clone)]
pub struct Resident {
	pub kind: String,
	pub id: i64,
	pub name: String,
	pub project_id: i64,
	pub status: String,
}

#[derive(Debug, Clone)]
pub struct EnvGroup {
	pub name: String,
	pub residents: Vec<Resident>,
}

#[derive(Debug, Clone)]
pub struct ProjectGroup {
	pub project_id: i64,
	pub name: String,
	pub environments: Vec<EnvGroup>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScannerLine {
	pub enabled: bool,
	pub scanned: i64,
}

#[derive(Debug, Clone)]
pub struct ServerCard {
	pub server: Server,
	/// Traefik runs on a server once an application there is running.
	pub proxy_running: bool,
	pub networks: Vec<NetworkLine>,
	pub groups: Vec<ProjectGroup>,
	/// `None` for someone who may not read scans.
	pub scanner: Option<ScannerLine>,
	pub points: Vec<ServerPoint>,
}

/// One row of the "what runs where" table.
#[derive(Debug, Clone)]
pub struct Place {
	pub kind: String,
	pub id: i64,
	pub name: String,
	pub project_id: i64,
	pub project_name: String,
	pub environment_name: String,
	pub server_id: ServerId,
	pub server_name: String,
	pub status: String,
	pub networks: Vec<String>,
	/// The public address of an application; `None` for what is only reachable inside.
	pub url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Overview {
	pub proxy: ProxyConfig,
	pub servers: Vec<ServerCard>,
	pub places: Vec<Place>,
	pub setup: SetupView,
}

/// The checklist as the person sees it.
#[derive(Debug, Clone)]
pub struct SetupView {
	pub steps: Vec<StepView>,
	pub progress: Progress,
	pub complete: bool,
	/// The step to open first: the first one still to do.
	pub current: Option<Step>,
	pub proxy: ProxyConfig,
}

impl SetupView {
	/// The checklist belongs on the overview until it is complete or dismissed.
	#[must_use]
	pub fn shown(&self) -> bool {
		!self.steps.is_empty() && !self.complete && !self.progress.dismissed
	}

	#[must_use]
	pub fn done_count(&self) -> usize {
		self.steps
			.iter()
			.filter(|s| s.state == setup::State::Done)
			.count()
	}
}

fn resource_for(kind: &str) -> Option<Resource> {
	Some(match kind {
		"application" => Resource::Application,
		"database" => Resource::Database,
		"model" => Resource::Model,
		"stack" => Resource::Stack,
		_ => return None,
	})
}

fn visible(perms: &Permissions) -> Visible {
	let can = |r| perms.can(Action::Read, r);
	Visible {
		server: can(Resource::Server),
		scan: can(Resource::Scan),
		project: can(Resource::Project),
		ssh_key: can(Resource::SshKey),
		git_provider: can(Resource::GitProvider),
		registry: can(Resource::Registry),
	}
}

fn proxy_config(state: &AppState) -> ProxyConfig {
	ProxyConfig {
		http_port: state.config.ingress_port,
		https_port: state.config.https_port,
		acme_email: state.config.acme_email.clone(),
	}
}

/// The setup checklist for this person; any member of the team may read it. The counts
/// are the team's own and the steps they may not read are left out, so a role that reads
/// nothing gets an empty list.
///
/// # Errors
/// `Forbidden` for a non-member (the page extractor has already given them a 404).
pub async fn setup(state: &AppState, user: UserId, team: TeamId) -> AppResult<SetupView> {
	let perms = granted(&state.db, user, team).await?;
	if !perms.is_member() {
		return Err(ServerError::Forbidden);
	}
	setup_for(&state.db, team, &perms, proxy_config(state)).await
}

async fn setup_for(
	db: &PgPool,
	team: TeamId,
	perms: &Permissions,
	proxy: ProxyConfig,
) -> AppResult<SetupView> {
	let facts = store::facts(db, team).await?;
	let progress = store::progress(db, team).await?;
	let steps = setup::derive(&facts, visible(perms), &progress);
	Ok(SetupView {
		complete: setup::complete(&steps),
		current: setup::first_todo(&steps),
		steps,
		progress,
		proxy,
	})
}

/// Skips an optional part of the setup, or takes the skip back; needs `update team`.
///
/// # Errors
/// `Forbidden` without `update team`.
pub async fn skip(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	item: Item,
	skipped: bool,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Team).await?;
	store::set_skipped(db, team, item, skipped).await?;
	Ok(())
}

/// Hides the checklist from the overview, or shows it again; needs `update team`.
///
/// # Errors
/// `Forbidden` without `update team`.
pub async fn dismiss(db: &PgPool, user: UserId, team: TeamId, dismissed: bool) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Team).await?;
	store::set_dismissed(db, team, dismissed).await?;
	Ok(())
}

fn url_of(placed: &Placed, server: &Server) -> Option<String> {
	(placed.kind == "application").then(|| {
		format!(
			"http://{}",
			applications::public_hostname(&placed.slug, server)
		)
	})
}

/// The residents of one server, grouped by project and then environment. `here` arrives
/// ordered by project and environment, so a change of either starts a new group.
fn group_residents(here: &[&Placed]) -> Vec<ProjectGroup> {
	let mut groups: Vec<ProjectGroup> = Vec::new();
	for p in here {
		if groups.last().is_none_or(|g| g.project_id != p.project_id) {
			groups.push(ProjectGroup {
				project_id: p.project_id,
				name: p.project_name.clone(),
				environments: Vec::new(),
			});
		}
		let Some(group) = groups.last_mut() else {
			continue;
		};
		if group
			.environments
			.last()
			.is_none_or(|e| e.name != p.environment_name)
		{
			group.environments.push(EnvGroup {
				name: p.environment_name.clone(),
				residents: Vec::new(),
			});
		}
		if let Some(env) = group.environments.last_mut() {
			env.residents.push(Resident {
				kind: p.kind.clone(),
				id: p.id,
				name: p.name.clone(),
				project_id: p.project_id,
				status: p.status.clone(),
			});
		}
	}
	groups
}

/// Every server with what runs on it, and the table of where each resource is; needs
/// `read server`. A resource sits on the networks of its server that it is a member of.
///
/// # Errors
/// `Forbidden` without `read server`.
pub async fn overview(state: &AppState, user: UserId, team: TeamId) -> AppResult<Overview> {
	authorize(&state.db, user, team, Action::Read, Resource::Server).await?;
	let perms = granted(&state.db, user, team).await?;
	let db = &state.db;
	let list = servers::list_by_team(db, team).await?;
	let placed: Vec<Placed> = store::placements(db, team)
		.await?
		.into_iter()
		.filter(|p| resource_for(&p.kind).is_some_and(|r| perms.can(Action::Read, r)))
		.collect();
	let scanners = if perms.can(Action::Read, Resource::Scan) {
		store::scanners(db, team).await?
	} else {
		Vec::new()
	};

	let mut cards = Vec::new();
	let mut rows = Vec::new();
	for server in list {
		let views = network_service::list(state, user, team, server.id)
			.await
			.unwrap_or_default();
		let mut networks_of: HashMap<(String, i64), Vec<String>> = HashMap::new();
		for view in &views {
			for m in &view.members {
				networks_of
					.entry((m.resource_kind.clone(), m.resource_id))
					.or_default()
					.push(view.network.name.clone());
			}
		}
		let here: Vec<&Placed> = placed.iter().filter(|p| p.server_id == server.id).collect();
		let groups = group_residents(&here);
		for p in &here {
			rows.push(Place {
				kind: p.kind.clone(),
				id: p.id,
				name: p.name.clone(),
				project_id: p.project_id,
				project_name: p.project_name.clone(),
				environment_name: p.environment_name.clone(),
				server_id: server.id,
				server_name: server.name.clone(),
				status: p.status.clone(),
				networks: networks_of
					.get(&(p.kind.clone(), p.id))
					.cloned()
					.unwrap_or_default(),
				url: url_of(p, &server),
			});
		}
		let proxy_running = here
			.iter()
			.any(|p| p.kind == "application" && p.status == "running");
		let points = metrics_service::server(db, user, team, server.id, Range::Day)
			.await
			.unwrap_or_default();
		cards.push(ServerCard {
			proxy_running,
			networks: views
				.iter()
				.map(|v| NetworkLine {
					name: v.network.name.clone(),
					kind: v.network.kind.clone(),
					internal: v.network.internal,
					members: v.members.len(),
				})
				.collect(),
			groups,
			scanner: scanners
				.iter()
				.find(|s| s.server_id == server.id)
				.map(|s| ScannerLine {
					enabled: s.enabled,
					scanned: s.scanned,
				}),
			points,
			server,
		});
	}
	let proxy = proxy_config(state);
	let setup = setup_for(db, team, &perms, proxy.clone()).await?;
	Ok(Overview {
		proxy,
		servers: cards,
		places: rows,
		setup,
	})
}

/// A step the caller may not see is `NotFound`, like any page of someone else's.
///
/// # Errors
/// `NotFound` for a step this person has no part in.
pub fn find_step(setup: &SetupView, step: Step) -> AppResult<&StepView> {
	setup
		.steps
		.iter()
		.find(|s| s.step == step)
		.ok_or(ServerError::NotFound)
}
