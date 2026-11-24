//! Firewall use cases for the web UI: authorize, then scope, then validate, then store or ask
//! the helper through the agent. Looking needs `read server`, every change `update server`:
//! the firewall is part of how the server is run.

use podkit_firewall::protocol::{PreviewInfo, Request, Response, StatusInfo};
use sqlx::PgPool;

use super::{
	MAX_RULES, NewRule, Rule, RuleInput, Settings, add_rule, delete_rule, parse_ssh_sources,
	policy, rule_count, rules, set_enabled, set_install_helper, set_ssh_sources, settings,
};
use crate::agents::AgentClient;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{FirewallRuleId, ServerId, TeamId, UserId};
use crate::scope;
use crate::servers::Server;
use crate::state::AppState;

/// Seconds the helper waits for a confirmation before it takes a change back.
pub const CONFIRM_SECS: u64 = 60;
/// Tries at reaching the server again after an apply, two seconds apart.
const VERIFY_TRIES: u32 = 4;
const VERIFY_PAUSE: std::time::Duration = if cfg!(test) {
	std::time::Duration::from_millis(10)
} else {
	std::time::Duration::from_secs(2)
};

/// What the helper on the server says about itself.
#[derive(Debug, Clone)]
pub enum Helper {
	/// There is no `podkit-firewall` on the server (or this user may not open its socket).
	NotInstalled,
	/// The agent or the helper did not answer; the message is safe to show.
	Unreachable(String),
	/// The helper answered.
	Ready(StatusInfo),
}

/// What the Firewall page shows.
#[derive(Debug, Clone)]
pub struct View {
	pub server: Server,
	pub helper: Helper,
	pub settings: Settings,
	pub rules: Vec<Rule>,
}

/// How an apply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
	/// In force and confirmed.
	Confirmed,
}

fn unexpected() -> ServerError {
	tracing::warn!("the firewall helper answered with something unexpected");
	ServerError::AgentUnavailable
}

/// The answer, with the helper's refusals as messages the page can show.
fn answer(response: Response) -> AppResult<Response> {
	match response {
		Response::NotInstalled => Err(ServerError::Validation(
			"The firewall helper is not installed on this server. Install it from the Overview page's Re-run bootstrap, or follow docs/firewall.md.".to_owned(),
		)),
		Response::Error { message } => Err(ServerError::Validation(message)),
		other => Ok(other),
	}
}

async fn client(state: &AppState, server: &Server) -> AppResult<AgentClient> {
	state.agents.client(state, server).await
}

async fn ask(client: &AgentClient, request: &Request) -> AppResult<Response> {
	answer(client.firewall(request).await?)
}

/// The page: the server, what the helper reports, the wanted settings and the allows; needs
/// `read server`. An agent or helper that does not answer is shown as such, not as an error.
pub async fn get(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<View> {
	authorize(&state.db, user, team, Action::Read, Resource::Server).await?;
	let server = scope::server(&state.db, team, server).await?;
	let helper = match client(state, &server).await {
		Err(_) => Helper::Unreachable("podkit cannot reach the agent of this server".to_owned()),
		Ok(c) => match c.firewall(&Request::Status).await {
			Ok(Response::Status(s)) => Helper::Ready(s),
			Ok(Response::NotInstalled) => Helper::NotInstalled,
			Ok(Response::Error { message }) | Err(ServerError::Validation(message)) => {
				Helper::Unreachable(message)
			}
			Ok(_) => {
				Helper::Unreachable("the helper answered with something unexpected".to_owned())
			}
			Err(_) => Helper::Unreachable("the agent did not answer".to_owned()),
		},
	};
	Ok(View {
		settings: settings(&state.db, server.id).await?,
		rules: rules(&state.db, server.id).await?,
		helper,
		server,
	})
}

async fn target_server(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Server> {
	authorize(db, user, team, Action::Update, Resource::Server).await?;
	scope::server(db, team, server).await
}

fn target(server: &Server) -> Target {
	Target::new("firewall", server.id).named(&server.name)
}

/// Adds an allow to the wanted rules (nothing is applied); needs `update server`.
pub async fn add(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	input: &RuleInput,
) -> AppResult<FirewallRuleId> {
	let server = target_server(&state.db, user, team, server).await?;
	let new: NewRule = input.parse()?;
	if rule_count(&state.db, server.id).await? >= MAX_RULES {
		return Err(ServerError::Validation(format!(
			"A server can hold {MAX_RULES} allows."
		)));
	}
	let id = add_rule(&state.db, server.id, &new).await?;
	audit::log(
		&state.db,
		user,
		team,
		"firewall.allow.add",
		target(&server),
		Diff::new()
			.set("proto", new.proto.as_str())
			.set("port", i64::from(new.port))
			.set("source", new.source.clone().unwrap_or_default()),
	)
	.await;
	Ok(id)
}

/// Removes an allow from the wanted rules; needs `update server`.
pub async fn remove(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	rule: FirewallRuleId,
) -> AppResult<()> {
	let server = target_server(&state.db, user, team, server).await?;
	let gone = delete_rule(&state.db, server.id, rule)
		.await?
		.ok_or(ServerError::NotFound)?;
	audit::log(
		&state.db,
		user,
		team,
		"firewall.allow.remove",
		target(&server),
		Diff::new()
			.set("proto", gone.proto)
			.set("port", i64::from(gone.port))
			.set("source", gone.source.unwrap_or_default()),
	)
	.await;
	Ok(())
}

/// Stores the ranges SSH is limited to (empty: anywhere); needs `update server`. Open SSH
/// sessions, podkit's own among them, stay allowed whatever the list says.
pub async fn save_ssh_sources(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	raw: &str,
) -> AppResult<()> {
	let server = target_server(&state.db, user, team, server).await?;
	let sources = parse_ssh_sources(raw)?;
	let before = settings(&state.db, server.id).await?.ssh_sources;
	set_ssh_sources(&state.db, server.id, &sources).await?;
	audit::log(
		&state.db,
		user,
		team,
		"firewall.ssh.set",
		target(&server),
		Diff::new().change("ssh_sources", before.join(" "), sources.join(" ")),
	)
	.await;
	Ok(())
}

async fn wanted(state: &AppState, server: &Server) -> AppResult<podkit_firewall::policy::Policy> {
	let settings = settings(&state.db, server.id).await?;
	let rules = rules(&state.db, server.id).await?;
	policy(server.ssh_port, &settings, &rules)
}

/// The script and the diff of the wanted rules against what is in force; needs
/// `update server` (it is the first step of an apply). Nothing changes on the server.
pub async fn preview(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<PreviewInfo> {
	let server = target_server(&state.db, user, team, server).await?;
	let policy = wanted(state, &server).await?;
	let client = client(state, &server).await?;
	match ask(&client, &Request::Preview { policy }).await? {
		Response::Preview(p) => Ok(p),
		_ => Err(unexpected()),
	}
}

/// Reaches the server over a fresh connection: the tunnel is dropped first, so a ruleset that
/// lets only the already open connection through does not pass.
async fn reach_again(state: &AppState, server: &Server) -> Option<AgentClient> {
	state.agents.forget(server.id);
	for attempt in 0..VERIFY_TRIES {
		if attempt > 0 {
			tokio::time::sleep(VERIFY_PAUSE).await;
		}
		if let Ok(c) = state.agents.client(state, server).await
			&& c.health().await.is_ok()
		{
			return Some(c);
		}
	}
	None
}

/// Applies the wanted rules with a confirm window; needs `update server`.
///
/// The helper applies them and starts a [`CONFIRM_SECS`] second timer. podkit then reaches
/// the server again over a new connection; only if that works does it confirm. If it does not,
/// it asks the helper to revert at once (and the timer reverts them anyway), and tells the team.
pub async fn apply(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Applied> {
	let server = target_server(&state.db, user, team, server).await?;
	let policy = wanted(state, &server).await?;
	let first = client(state, &server).await?;
	let request = Request::Apply {
		policy,
		confirm_secs: Some(CONFIRM_SECS),
	};
	let Response::Applied { token, .. } = ask(&first, &request).await? else {
		return Err(unexpected());
	};
	let Some(second) = reach_again(state, &server).await else {
		let _ = first.firewall(&Request::Rollback).await;
		let why = "podkit could not reach the server again after the change, so it was reverted";
		crate::notifications::hooks::firewall_reverted(state, server.id.0, why).await;
		audit::log(
			&state.db,
			user,
			team,
			"firewall.revert",
			target(&server),
			Diff::new().set("reason", "not reachable after the change"),
		)
		.await;
		return Err(ServerError::Validation(format!("{why}.")));
	};
	if let Err(e) = ask(&second, &Request::Confirm { token }).await {
		crate::notifications::hooks::firewall_reverted(
			state,
			server.id.0,
			"the confirmation did not reach the helper in time",
		)
		.await;
		return Err(e);
	}
	set_enabled(&state.db, server.id, true).await?;
	audit::log(
		&state.db,
		user,
		team,
		"firewall.apply",
		target(&server),
		Diff::new().set("confirmed", true),
	)
	.await;
	Ok(Applied::Confirmed)
}

/// Removes the podkit table: the firewall is off; needs `update server`.
pub async fn disable(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<()> {
	let server = target_server(&state.db, user, team, server).await?;
	let client = client(state, &server).await?;
	ask(&client, &Request::Disable).await?;
	set_enabled(&state.db, server.id, false).await?;
	audit::log(
		&state.db,
		user,
		team,
		"firewall.disable",
		target(&server),
		Diff::new().set("enabled", false),
	)
	.await;
	Ok(())
}

/// Opts the server in to the root helper and runs the bootstrap that installs it; needs
/// `update server`. The helper goes in with `sudo` over the server's SSH login, so that login
/// needs passwordless sudo (or be root). The local server's helper is installed by hand, see
/// `docs/firewall.md`.
pub async fn install_helper(
	state: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<()> {
	let server = target_server(&state.db, user, team, server).await?;
	if server.is_local {
		return Err(ServerError::Validation(
			"The helper on the podkit host is installed by hand; see docs/firewall.md.".to_owned(),
		));
	}
	set_install_helper(&state.db, server.id, true).await?;
	audit::log(
		&state.db,
		user,
		team,
		"firewall.install",
		target(&server),
		Diff::new().set("install_helper", true),
	)
	.await;
	crate::servers::service::bootstrap(state, user, team, server.id, false).await?;
	Ok(())
}
